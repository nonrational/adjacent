use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair, KeyUsagePurpose,
    SanType,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig;

use crate::paths;

/// SANs the leaf cert covers. Wildcard `*.adj.ac` matches one label so a `adj.ac` apex SAN is
/// included alongside it — Adjacent's landing page already lives at the apex.
const WILDCARD_HOST: &str = "*.adj.ac";
const APEX_HOST: &str = "adj.ac";

pub const CA_CERT_FILENAME: &str = "ca.crt";
pub const CA_KEY_FILENAME: &str = "ca.key";
pub const LEAF_CERT_FILENAME: &str = "cert.crt";
pub const LEAF_KEY_FILENAME: &str = "cert.key";

pub fn ca_cert_path() -> Result<PathBuf> {
    Ok(paths::home_dir()?.join(CA_CERT_FILENAME))
}

pub fn ca_key_path() -> Result<PathBuf> {
    Ok(paths::home_dir()?.join(CA_KEY_FILENAME))
}

pub fn leaf_cert_path() -> Result<PathBuf> {
    Ok(paths::home_dir()?.join(LEAF_CERT_FILENAME))
}

pub fn leaf_key_path() -> Result<PathBuf> {
    Ok(paths::home_dir()?.join(LEAF_KEY_FILENAME))
}

/// Generate a fresh local CA and write `ca.crt` / `ca.key` to `~/.adjacent/`. Idempotent in the
/// sense that callers should check `ca_exists` first — this overwrites unconditionally.
pub fn generate_ca() -> Result<()> {
    paths::ensure_dirs()?;
    let (params, key_pair) = build_ca_params()?;
    let signed = params
        .self_signed(&key_pair)
        .context("self-signing CA cert")?;
    let cert_path = ca_cert_path()?;
    let key_path = ca_key_path()?;
    fs::write(&cert_path, signed.pem()).with_context(|| format!("writing {}", cert_path.display()))?;
    write_private_pem(&key_path, &key_pair.serialize_pem())?;
    // Leaf gets regenerated whenever the CA changes — anything currently on disk was signed by
    // the previous CA and will not chain to the new root. Drop it eagerly so the daemon issues
    // a fresh one on next startup.
    let _ = fs::remove_file(leaf_cert_path()?);
    let _ = fs::remove_file(leaf_key_path()?);
    Ok(())
}

pub fn ca_exists() -> Result<bool> {
    Ok(ca_cert_path()?.exists() && ca_key_path()?.exists())
}

/// Build a `rustls::ServerConfig` from disk, generating the leaf cert on demand. Caller is
/// expected to surface errors (HTTPS listener startup is best-effort: no CA → log and skip).
pub fn server_config() -> Result<Arc<ServerConfig>> {
    if !ca_exists()? {
        return Err(anyhow!(
            "local CA not found — run `adj install-ca` to generate one"
        ));
    }
    let (leaf_cert_pem, leaf_key_pem) = ensure_leaf()?;
    let chain = parse_cert_chain(&leaf_cert_pem).context("parsing leaf certificate chain")?;
    let key = parse_private_key(&leaf_key_pem).context("parsing leaf private key")?;
    // rustls 0.23 with the default `aws_lc_rs` feature auto-installs the process default crypto
    // provider on first builder() call, so no explicit install_default() is needed here.
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .context("building rustls ServerConfig")?;
    Ok(Arc::new(config))
}

/// Read the leaf cert + key from disk, regenerating them if missing. We don't try to detect
/// CA mismatches at boot — `generate_ca` deletes the stale leaf, so a missing-leaf branch
/// covers both fresh-install and post-rotation paths with one mechanism.
fn ensure_leaf() -> Result<(String, String)> {
    let cert_path = leaf_cert_path()?;
    let key_path = leaf_key_path()?;
    if cert_path.exists() && key_path.exists() {
        let cert = fs::read_to_string(&cert_path)
            .with_context(|| format!("reading {}", cert_path.display()))?;
        let key = fs::read_to_string(&key_path)
            .with_context(|| format!("reading {}", key_path.display()))?;
        return Ok((cert, key));
    }
    issue_leaf()
}

fn issue_leaf() -> Result<(String, String)> {
    let ca_cert_pem = fs::read_to_string(ca_cert_path()?).context("reading CA cert")?;
    let ca_key_pem = fs::read_to_string(ca_key_path()?).context("reading CA key")?;
    let ca_key = KeyPair::from_pem(&ca_key_pem).context("parsing CA key")?;
    let ca_params = CertificateParams::from_ca_cert_pem(&ca_cert_pem)
        .context("parsing CA cert params for re-issue")?;
    let ca_signed = ca_params
        .self_signed(&ca_key)
        .context("rebuilding CA from disk")?;

    let mut leaf_params = CertificateParams::new(vec![
        WILDCARD_HOST.to_string(),
        APEX_HOST.to_string(),
    ])
    .context("building leaf cert params")?;
    leaf_params.distinguished_name = leaf_dn();
    leaf_params.subject_alt_names = vec![
        SanType::DnsName(WILDCARD_HOST.try_into().context("wildcard SAN")?),
        SanType::DnsName(APEX_HOST.try_into().context("apex SAN")?),
    ];
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

fn build_ca_params() -> Result<(CertificateParams, KeyPair)> {
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
    let key_pair = KeyPair::generate().context("generating CA key pair")?;
    Ok((params, key_pair))
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
        f();
        std::env::remove_var("ADJACENT_HOME");
    }

    #[test]
    fn generate_ca_writes_cert_and_key() {
        with_temp_home(|| {
            generate_ca().expect("generate_ca");
            assert!(ca_cert_path().unwrap().exists());
            assert!(ca_key_path().unwrap().exists());
            let pem = std::fs::read_to_string(ca_cert_path().unwrap()).unwrap();
            assert!(pem.contains("BEGIN CERTIFICATE"));
        });
    }

    #[test]
    fn server_config_errors_without_ca() {
        with_temp_home(|| {
            let err = server_config().unwrap_err();
            assert!(format!("{err}").contains("install-ca"));
        });
    }

    #[test]
    fn server_config_succeeds_after_generate() {
        with_temp_home(|| {
            generate_ca().expect("generate_ca");
            let _cfg = server_config().expect("server_config");
            assert!(leaf_cert_path().unwrap().exists());
            assert!(leaf_key_path().unwrap().exists());
        });
    }
}
