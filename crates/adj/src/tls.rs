use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, GeneralSubtree, IsCa, KeyPair,
    KeyUsagePurpose, NameConstraints, SanType,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls::ServerConfig;

use crate::paths;
use crate::registry::{self, Registry};

mod keychain;

pub(crate) use keychain::delete as delete_keychain_ca;
pub(crate) use keychain::load as load_keychain_ca;

/// Tear down the local CA: remove the Keychain-resident key and any cached on-disk files.
/// Idempotent — safe to call when no CA is present. Used by `install-ca --reset` and by
/// integration tests during teardown so each run starts from a clean Keychain slate.
pub fn delete_ca() -> Result<()> {
    delete_keychain_ca()?;
    let _ = fs::remove_file(ca_cert_path()?);
    let _ = fs::remove_file(leaf_cert_path()?);
    let _ = fs::remove_file(leaf_key_path()?);
    Ok(())
}

/// SANs the leaf cert covers. Wildcard `*.adj.ac` matches one label so a `adj.ac` apex SAN is
/// included alongside it — Adjacent's landing page already lives at the apex.
const WILDCARD_HOST: &str = "*.adj.ac";
const APEX_HOST: &str = "adj.ac";

/// The leaf SAN set for a registry snapshot: the v1 apex + single-label wildcard, plus a
/// `*.<base>.adj.ac` wildcard per distinct base name so worktree instances
/// (`<label>.<base>.adj.ac`) validate. A wildcard matches exactly one label, so the per-base
/// entries can't be folded into `*.adj.ac`. Deterministic order (apex, wildcard, sorted bases)
/// makes set comparison against an issued cert a plain Vec equality.
pub fn registry_sans(reg: &Registry) -> Vec<String> {
    let mut sans = vec![APEX_HOST.to_string(), WILDCARD_HOST.to_string()];
    let mut bases: Vec<&str> = reg.apps.keys().map(|k| registry::base_name(k)).collect();
    bases.sort_unstable();
    bases.dedup();
    for base in bases {
        sans.push(format!("*.{base}.adj.ac"));
    }
    sans
}

/// The CA's nameConstraints `permitted_subtrees` is scoped to this DNS suffix. Per RFC 5280
/// § 4.2.1.10 a DNS-name subtree matches the bare name and any name ending in `.<suffix>`, so
/// `adj.ac` covers both the apex SAN and the `*.adj.ac` wildcard. The constraint extension is
/// emitted critical (rcgen does this) — Safari, Chrome, Firefox, and macOS Security framework all
/// reject leaves outside the subtree, so a stolen CA cert cannot mint trusted certs for, say,
/// `google.com`.
const NAME_CONSTRAINT_DNS: &str = "adj.ac";

pub const CA_CERT_FILENAME: &str = "ca.crt";
/// Legacy on-disk private-key path. We no longer write to it (the CA key lives in the login
/// keychain, marked non-extractable), but `install-ca` looks for it to detect a pre-keychain
/// install and prompt the user to scrub the old material.
pub const LEGACY_CA_KEY_FILENAME: &str = "ca.key";
pub const LEAF_CERT_FILENAME: &str = "cert.crt";
pub const LEAF_KEY_FILENAME: &str = "cert.key";

pub fn ca_cert_path() -> Result<PathBuf> {
    Ok(paths::home_dir()?.join(CA_CERT_FILENAME))
}

pub fn legacy_ca_key_path() -> Result<PathBuf> {
    Ok(paths::home_dir()?.join(LEGACY_CA_KEY_FILENAME))
}

pub fn leaf_cert_path() -> Result<PathBuf> {
    Ok(paths::home_dir()?.join(LEAF_CERT_FILENAME))
}

pub fn leaf_key_path() -> Result<PathBuf> {
    Ok(paths::home_dir()?.join(LEAF_KEY_FILENAME))
}

/// Identify the Keychain-resident CA key for humans inspecting Keychain Access. Surfaced by
/// `install-ca` so the printed banner can tell the user what to look for.
pub fn ca_keychain_label() -> Result<String> {
    keychain::ca_label()
}

/// Generate a fresh local CA: create the Keychain-resident private key (non-extractable),
/// self-sign a CA cert with name constraints scoped to `adj.ac`, write the public cert to
/// `~/.adjacent/ca.crt`. Callers should gate this on `ca_exists` — generating twice replaces
/// the Keychain entry and invalidates any leaf already trusted by clients.
pub fn generate_ca() -> Result<()> {
    paths::ensure_dirs()?;
    let ca_handle = keychain::generate().context("creating Keychain CA key")?;
    let ca_key = ca_handle.into_rcgen_keypair()?;
    let params = build_ca_params();
    let signed = params
        .self_signed(&ca_key)
        .context("self-signing CA cert")?;
    let cert_path = ca_cert_path()?;
    fs::write(&cert_path, signed.pem())
        .with_context(|| format!("writing {}", cert_path.display()))?;
    // Leaf gets regenerated whenever the CA changes — anything currently on disk was signed by
    // the previous CA and will not chain to the new root. Drop it eagerly so the daemon issues
    // a fresh one on next startup.
    let _ = fs::remove_file(leaf_cert_path()?);
    let _ = fs::remove_file(leaf_key_path()?);
    Ok(())
}

/// Both halves of the CA — public cert on disk AND a usable signing handle in the login
/// keychain — must be present. A torn state (cert without key, or key without cert) means we
/// can't sign leaves and should be treated as no-CA; `install-ca` will then regenerate.
pub fn ca_exists() -> Result<bool> {
    if !ca_cert_path()?.exists() {
        return Ok(false);
    }
    Ok(keychain::load()?.is_some())
}

/// Serves the daemon's leaf cert and re-issues it when the registry's SAN set changes, so a
/// newly added worktree instance gets a valid cert without an HTTPS-listener restart.
#[derive(Debug)]
pub struct LeafResolver {
    current: RwLock<Arc<CertifiedKey>>,
    /// SANs baked into `current`; compared on reload so an unchanged registry skips the
    /// keychain signature entirely.
    sans: RwLock<Vec<String>>,
}

impl LeafResolver {
    /// Build the resolver from the on-disk CA, issuing a leaf that covers the current
    /// registry. Errors when the CA is missing — callers treat that as "HTTPS not opted in".
    pub fn new() -> Result<Arc<Self>> {
        if !ca_exists()? {
            return Err(anyhow!(
                "local CA not found — run `adj install-ca` to generate one"
            ));
        }
        let sans = registry_sans(&Registry::load()?);
        let key = certified_key_for(&sans)?;
        Ok(Arc::new(Self {
            current: RwLock::new(Arc::new(key)),
            sans: RwLock::new(sans),
        }))
    }

    /// Recompute the SAN set from the registry; re-issue and swap the served cert if changed.
    pub fn reload(&self) -> Result<()> {
        let sans = registry_sans(&Registry::load()?);
        if *self.sans.read().expect("sans lock") == sans {
            return Ok(());
        }
        let key = certified_key_for(&sans)?;
        *self.current.write().expect("cert lock") = Arc::new(key);
        *self.sans.write().expect("sans lock") = sans;
        Ok(())
    }
}

impl ResolvesServerCert for LeafResolver {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.current.read().expect("cert lock").clone())
    }
}

fn certified_key_for(sans: &[String]) -> Result<CertifiedKey> {
    let (cert_pem, key_pem) = ensure_leaf(sans)?;
    let chain = parse_cert_chain(&cert_pem).context("parsing leaf certificate chain")?;
    let key_der = parse_private_key(&key_pem).context("parsing leaf private key")?;
    // rustls 0.23 with the default `aws_lc_rs` feature auto-installs the process default
    // crypto provider, matching the previous with_single_cert behavior.
    let signing_key = rustls::crypto::aws_lc_rs::sign::any_supported_type(&key_der)
        .context("building leaf signing key")?;
    Ok(CertifiedKey::new(chain, signing_key))
}

/// Build a `rustls::ServerConfig` around the hot-swapping resolver. Caller is expected to
/// surface errors (HTTPS listener startup is best-effort: no CA → log and skip).
pub fn server_config(resolver: Arc<LeafResolver>) -> Result<Arc<ServerConfig>> {
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_cert_resolver(resolver);
    Ok(Arc::new(config))
}

/// Read the leaf cert + key from disk, re-issuing when missing OR when the on-disk SAN set no
/// longer matches the desired one (an app was added/removed since issuance, or the leaf
/// predates worktree instances). `generate_ca` still deletes the leaf on CA rotation, so this
/// single mechanism covers fresh-install, post-rotation, and SAN-drift paths.
fn ensure_leaf(sans: &[String]) -> Result<(String, String)> {
    let cert_path = leaf_cert_path()?;
    let key_path = leaf_key_path()?;
    if cert_path.exists() && key_path.exists() {
        let cert = fs::read_to_string(&cert_path)
            .with_context(|| format!("reading {}", cert_path.display()))?;
        let key = fs::read_to_string(&key_path)
            .with_context(|| format!("reading {}", key_path.display()))?;
        if leaf_covers(&cert, sans)? {
            return Ok((cert, key));
        }
    }
    issue_leaf(sans)
}

/// True when the leaf's DNS SANs equal the desired set exactly (order-insensitive).
fn leaf_covers(cert_pem: &str, sans: &[String]) -> Result<bool> {
    let (_, parsed) = x509_parser::pem::parse_x509_pem(cert_pem.as_bytes())
        .map_err(|e| anyhow!("parsing leaf PEM: {e}"))?;
    let cert = parsed
        .parse_x509()
        .map_err(|e| anyhow!("parsing leaf X.509: {e}"))?;
    let mut have: Vec<String> = cert
        .subject_alternative_name()
        .map_err(|e| anyhow!("reading leaf SANs: {e}"))?
        .map(|ext| {
            ext.value
                .general_names
                .iter()
                .filter_map(|gn| match gn {
                    x509_parser::extensions::GeneralName::DNSName(n) => Some(n.to_string()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    have.sort_unstable();
    let mut want: Vec<String> = sans.to_vec();
    want.sort_unstable();
    Ok(have == want)
}

fn issue_leaf(sans: &[String]) -> Result<(String, String)> {
    let ca_cert_pem = fs::read_to_string(ca_cert_path()?).context("reading CA cert")?;
    let ca_handle = keychain::load()
        .context("loading Keychain CA key")?
        .ok_or_else(|| anyhow!("no Keychain CA key — run `adj install-ca`"))?;
    let ca_key = ca_handle.into_rcgen_keypair()?;
    // rcgen needs the CA's *parsed* CertificateParams + a `Certificate` object so it can copy
    // issuer DN + SKI into the leaf. `from_ca_cert_pem` rebuilds the params; re-self-signing
    // with the Keychain key gives us an equivalent Certificate handle to pass to `signed_by`.
    let ca_params = CertificateParams::from_ca_cert_pem(&ca_cert_pem)
        .context("parsing CA cert params for re-issue")?;
    let ca_signed = ca_params
        .self_signed(&ca_key)
        .context("rebuilding CA from disk")?;

    let mut leaf_params =
        CertificateParams::new(sans.to_vec()).context("building leaf cert params")?;
    leaf_params.distinguished_name = leaf_dn();
    leaf_params.subject_alt_names = sans
        .iter()
        .map(|s| {
            Ok(SanType::DnsName(
                s.as_str().try_into().with_context(|| format!("SAN `{s}`"))?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let leaf_key = KeyPair::generate().context("generating leaf key pair")?;
    let leaf_cert = leaf_params
        .signed_by(&leaf_key, &ca_signed, &ca_key)
        .context("signing leaf with CA")?;

    let cert_pem = leaf_cert.pem();
    let key_pem = leaf_key.serialize_pem();
    fs::write(leaf_cert_path()?, &cert_pem).context("writing leaf cert")?;
    write_private_pem(&leaf_key_path()?, &key_pem)?;
    Ok((cert_pem, key_pem))
}

fn build_ca_params() -> CertificateParams {
    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "Adjacent local");
    dn.push(DnType::OrganizationName, "Adjacent");
    // Embed seconds-since-epoch in the OU so repeated installs are distinguishable in Keychain
    // Access — humans can tell two "Adjacent local" entries apart by clicking through.
    if let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) {
        dn.push(DnType::OrganizationalUnitName, format!("ca-{}", now.as_secs()));
    }
    params.distinguished_name = dn;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    // Scope the trust anchor to the only DNS namespace Adjacent uses. Without this a stolen CA
    // cert + key is a general-purpose root, accepted by browsers for any domain. With it,
    // RFC 5280 § 4.2.1.10 limits the CA to `adj.ac` and `*.adj.ac` and the major trust stores
    // enforce.
    params.name_constraints = Some(NameConstraints {
        permitted_subtrees: vec![GeneralSubtree::DnsName(NAME_CONSTRAINT_DNS.to_string())],
        excluded_subtrees: vec![],
    });
    params
}

fn leaf_dn() -> DistinguishedName {
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "Adjacent wildcard");
    dn.push(DnType::OrganizationName, "Adjacent");
    dn
}

fn parse_cert_chain(pem: &str) -> Result<Vec<CertificateDer<'static>>> {
    let mut reader = BufReader::new(pem.as_bytes());
    let chain = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .context("parsing PEM cert chain")?;
    if chain.is_empty() {
        return Err(anyhow!("no certificates found in PEM"));
    }
    Ok(chain)
}

fn parse_private_key(pem: &str) -> Result<PrivateKeyDer<'static>> {
    let mut reader = BufReader::new(pem.as_bytes());
    rustls_pemfile::private_key(&mut reader)
        .context("parsing PEM private key")?
        .ok_or_else(|| anyhow!("no private key found in PEM"))
}

#[cfg(unix)]
fn write_private_pem(path: &Path, contents: &str) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true).mode(0o600);
    let mut file = opts.open(path).with_context(|| format!("opening {}", path.display()))?;
    use std::io::Write;
    file.write_all(contents.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private_pem(path: &Path, contents: &str) -> Result<()> {
    fs::write(path, contents).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    // `ADJACENT_HOME` is a process-global env var; cargo runs unit tests in parallel by default,
    // so two tests racing on set/remove will leak state across each other (the symptom is a
    // tempdir going out of scope while another test still expects it to exist). Serialize the
    // whole `with_temp_home` block to keep each test deterministic.
    static HOME_LOCK: Mutex<()> = Mutex::new(());

    fn with_temp_home<F: FnOnce()>(f: F) {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().expect("tempdir");
        std::env::set_var("ADJACENT_HOME", tmp.path());
        // Each test gets a unique ADJACENT_HOME → unique Keychain label. Wrap the work in a
        // guard so a panicking test still tears its Keychain entry down — otherwise we'd
        // accumulate orphan entries in the dev's Keychain Access on every red run.
        struct Cleanup;
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = delete_keychain_ca();
            }
        }
        let _cleanup = Cleanup;
        f();
        // Drop runs _cleanup first (LIFO), then we remove the env var.
        drop(_cleanup);
        std::env::remove_var("ADJACENT_HOME");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn generate_ca_writes_cert_and_provisions_keychain_key() {
        with_temp_home(|| {
            generate_ca().expect("generate_ca");
            assert!(ca_cert_path().unwrap().exists());
            // No on-disk private key any more — the login keychain owns it.
            assert!(!legacy_ca_key_path().unwrap().exists());
            assert!(ca_exists().expect("ca_exists"));
            let pem = std::fs::read_to_string(ca_cert_path().unwrap()).unwrap();
            assert!(pem.contains("BEGIN CERTIFICATE"));
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ca_cert_has_name_constraint_for_adj_ac() {
        with_temp_home(|| {
            generate_ca().expect("generate_ca");
            let pem = std::fs::read_to_string(ca_cert_path().unwrap()).unwrap();
            let (_, parsed) = x509_parser::pem::parse_x509_pem(pem.as_bytes()).expect("pem");
            let cert = parsed.parse_x509().expect("x509");
            let nc = cert
                .name_constraints()
                .expect("name_constraints lookup")
                .expect("nameConstraints extension present");
            // Critical bit MUST be set per RFC 5280; rcgen does this — assert so a future
            // regression in our params shows up here.
            let ext = cert
                .extensions()
                .iter()
                .find(|e| e.oid == x509_parser::oid_registry::OID_X509_EXT_NAME_CONSTRAINTS)
                .expect("nameConstraints extension");
            assert!(ext.critical, "nameConstraints must be critical");
            let permitted = nc.value.permitted_subtrees.as_deref().unwrap_or(&[]);
            assert_eq!(permitted.len(), 1, "expected one permitted subtree");
            let only = &permitted[0];
            match &only.base {
                x509_parser::extensions::GeneralName::DNSName(name) => {
                    assert_eq!(*name, "adj.ac");
                }
                other => panic!("expected DNSName subtree, got {other:?}"),
            }
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn server_config_errors_without_ca() {
        with_temp_home(|| {
            let err = LeafResolver::new().unwrap_err();
            assert!(format!("{err}").contains("install-ca"));
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn server_config_succeeds_after_generate() {
        with_temp_home(|| {
            generate_ca().expect("generate_ca");
            let resolver = LeafResolver::new().expect("resolver");
            let _cfg = server_config(resolver).expect("server_config");
            assert!(leaf_cert_path().unwrap().exists());
            assert!(leaf_key_path().unwrap().exists());
        });
    }

    #[test]
    fn registry_sans_adds_wildcard_per_base() {
        use crate::registry::AppEntry;
        let mut reg = Registry::default();
        reg.insert("site".into(), AppEntry { path: "/tmp/a".into() });
        reg.insert("feature-x.site".into(), AppEntry { path: "/tmp/b".into() });
        reg.insert("api".into(), AppEntry { path: "/tmp/c".into() });
        let expected: Vec<String> = ["adj.ac", "*.adj.ac", "*.api.adj.ac", "*.site.adj.ac"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(registry_sans(&reg), expected);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn leaf_reissues_when_san_set_changes() {
        with_temp_home(|| {
            generate_ca().expect("generate_ca");
            let base: Vec<String> = vec!["adj.ac".into(), "*.adj.ac".into()];
            let (pem1, _) = ensure_leaf(&base).expect("first issue");
            // Same set → the cached leaf comes back byte-identical (no needless keychain work).
            let (pem1b, _) = ensure_leaf(&base).expect("cached");
            assert_eq!(pem1, pem1b);
            let widened: Vec<String> =
                vec!["adj.ac".into(), "*.adj.ac".into(), "*.site.adj.ac".into()];
            let (pem2, _) = ensure_leaf(&widened).expect("re-issue");
            assert_ne!(pem1, pem2, "SAN change must re-issue the leaf");
            assert!(leaf_covers(&pem2, &widened).expect("parse"));
            assert!(!leaf_covers(&pem2, &base).expect("parse"));
        });
    }

    /// Regression guard for the `build_ca_params` timestamp + `from_ca_cert_pem` interaction.
    /// `build_ca_params` embeds `now.as_secs()` in the OU, so calling it twice yields different
    /// subjects. `issue_leaf` must re-parse the *on-disk* CA (not rebuild from scratch) so the
    /// leaf's Issuer DN matches the trust-anchor's Subject DN — otherwise leaves wouldn't chain.
    /// Without this test, a refactor that swaps `from_ca_cert_pem` for a `build_ca_params` call
    /// would pass other tests and silently break HTTPS validation in browsers.
    #[cfg(target_os = "macos")]
    #[test]
    fn leaf_issuer_matches_ca_subject_after_reissue() {
        use std::time::Duration;
        with_temp_home(|| {
            generate_ca().expect("generate_ca");
            // Force a measurable gap so any accidental rebuild path would land on a different
            // `now.as_secs()` OU and the assertion would fail.
            std::thread::sleep(Duration::from_millis(1100));
            let _resolver = LeafResolver::new().expect("resolver");
            let ca_pem = std::fs::read_to_string(ca_cert_path().unwrap()).unwrap();
            let leaf_pem = std::fs::read_to_string(leaf_cert_path().unwrap()).unwrap();
            let (_, ca_block) =
                x509_parser::pem::parse_x509_pem(ca_pem.as_bytes()).expect("ca pem");
            let ca = ca_block.parse_x509().expect("ca x509");
            let (_, leaf_block) =
                x509_parser::pem::parse_x509_pem(leaf_pem.as_bytes()).expect("leaf pem");
            let leaf = leaf_block.parse_x509().expect("leaf x509");
            assert_eq!(
                leaf.issuer().to_string(),
                ca.subject().to_string(),
                "leaf Issuer DN must equal CA Subject DN or chain validation fails"
            );
        });
    }
}
