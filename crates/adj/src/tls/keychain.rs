//! Adjacent's CA private key lives in the macOS login keychain as a software ECDSA P-256 key,
//! marked `kSecAttrIsExtractable=false`. Signing routes through `SecKeyCreateSignature` via
//! rcgen's `RemoteKeyPair` trait, so the private key never enters process memory. The
//! framework refuses to return the bytes through ordinary tooling (`security export`, Keychain
//! Access UI export, `SecItemCopyMatching(kSecReturnData=true)`).
//!
//! *Not* a hardware boundary. `kSecAttrIsExtractable=false` is a software promise enforced by
//! the Security framework — a determined attacker with the user's login password and
//! framework-level access can probably still pull the bytes out via legacy `SecKeychain` APIs
//! or platform quirks. Secure Enclave is the only thing that gives the strong claim; moving
//! the CA key into SE is tracked in #42. The current setup is a meaningful step up from
//! "PEM file at mode 0600" — no cleartext bytes on disk, no accidental file copies, no
//! Keychain Access UI export — but it is not absolute.
//!
//! The lookup key for the keychain entry is `kSecAttrLabel`. We derive a stable label per
//! `ADJACENT_HOME` so a real install (`~/.adjacent`) and N test invocations (each with a unique
//! tempdir) coexist in the user's keychain without colliding. Tests clean up via [`delete`] on
//! teardown. `SecKeyCreateRandomKey` writes both a private *and* a public keychain item; the
//! delete path removes both classes by label so a reset leaves nothing behind.

use anyhow::Result;

use crate::paths;

/// Stable per-install label used to find the CA key on second and subsequent runs. Derived from
/// `home_dir()` (which honors `ADJACENT_HOME`), so the default install gets a recognizable
/// name in Keychain Access and tests get unique labels automatically.
pub fn ca_label() -> Result<String> {
    let home = paths::home_dir()?;
    let canonical = home.canonicalize().unwrap_or_else(|_| home.clone());
    let suffix = canonical.to_string_lossy();
    if let Ok(default_home) = paths::default_home_dir() {
        if canonical == default_home {
            return Ok("Adjacent local CA".to_string());
        }
    }
    Ok(format!("Adjacent local CA ({suffix})"))
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use anyhow::anyhow;
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;
    use rcgen::{
        Error as RcgenError, KeyPair, RemoteKeyPair, SignatureAlgorithm, PKCS_ECDSA_P256_SHA256,
    };
    use security_framework::item::{
        ItemClass, ItemSearchOptions, KeyClass, Limit, Reference, SearchResult,
    };
    use security_framework::key::{Algorithm, SecKey};
    use security_framework_sys::base::SecKeyRef;
    use security_framework_sys::item::{
        kSecAttrIsPermanent, kSecAttrKeySizeInBits, kSecAttrKeyType,
        kSecAttrKeyTypeECSECPrimeRandom, kSecAttrLabel, kSecPrivateKeyAttrs, kSecPublicKeyAttrs,
    };
    use security_framework_sys::key::SecKeyCreateRandomKey;

    // `kSecAttrIsExtractable` controls whether the keychain item's secret can be exported by
    // calls like `SecItemCopyMatching(kSecReturnData=true)` or by Keychain Access's right-click
    // → Export. Setting it false is what makes the key meaningfully better than a file on disk:
    // the bytes exist in the encrypted keychain only, and the framework refuses to hand them
    // out. The constant isn't in security-framework-sys, so we link it directly from the
    // Security framework (already linked via security-framework-sys).
    extern "C" {
        static kSecAttrIsExtractable: core_foundation_sys::string::CFStringRef;
    }

    /// Adjacent's view of a Keychain-resident ECDSA P-256 keypair. Wraps the opaque `SecKey`
    /// handle and a cached copy of the public key in SEC1 uncompressed form (the shape rcgen
    /// wants for `RemoteKeyPair::public_key`).
    pub struct KeychainKey {
        private: SecKey,
        public_bytes: Vec<u8>,
    }

    impl KeychainKey {
        /// Wrap an existing `SecKey` and pull its public-key bytes once. Errors surface if the
        /// public half can't be recovered from the handle (would mean a corrupted Keychain
        /// entry).
        fn from_seckey(key: SecKey) -> Result<Self> {
            let public = key
                .public_key()
                .ok_or_else(|| anyhow!("CA key has no recoverable public key"))?;
            let data = public
                .external_representation()
                .ok_or_else(|| anyhow!("CA public key external representation unavailable"))?;
            Ok(Self {
                private: key,
                public_bytes: data.to_vec(),
            })
        }

        /// Hand the keychain handle to rcgen as a [`KeyPair`]. rcgen signs through the
        /// `RemoteKeyPair` trait below; the private key bytes never leave the keychain process
        /// boundary.
        pub fn into_rcgen_keypair(self) -> Result<KeyPair> {
            KeyPair::from_remote(Box::new(self))
                .map_err(|e| anyhow!("constructing rcgen KeyPair from Keychain handle: {e}"))
        }

        /// Sign a fixed canary buffer with the keychain key. Used by `adj doctor` to confirm the
        /// current binary's cdhash satisfies the key's ACL. `SecKeyCreateSignature` exercises a
        /// different auth path than `SecKeyCopyExternalRepresentation`, so a "load" success does
        /// not guarantee "sign" success — this gives the doctor a true end-to-end signing probe.
        pub fn sign_canary(&self) -> Result<()> {
            self.private
                .create_signature(
                    Algorithm::ECDSASignatureMessageX962SHA256,
                    b"adj-doctor-canary",
                )
                .map(|_| ())
                .map_err(|e| anyhow!("keychain sign canary failed: {e}"))
        }
    }

    impl RemoteKeyPair for KeychainKey {
        fn public_key(&self) -> &[u8] {
            &self.public_bytes
        }

        fn algorithm(&self) -> &'static SignatureAlgorithm {
            &PKCS_ECDSA_P256_SHA256
        }

        fn sign(&self, msg: &[u8]) -> std::result::Result<Vec<u8>, RcgenError> {
            self.private
                .create_signature(Algorithm::ECDSASignatureMessageX962SHA256, msg)
                .map_err(|cf_err| {
                    // rcgen's RemoteKeyError has no payload — log the CFError so it's not lost.
                    tracing::error!(error = %cf_err, "Keychain signing failed");
                    RcgenError::RemoteKeyError
                })
        }
    }

    /// Generate a fresh non-extractable CA key in the login keychain under the install-specific
    /// label. If a previous key with the same label exists, delete it first so duplicate-label
    /// search results don't confuse the load path. Returns the wrapped handle for immediate use
    /// by `generate_ca`.
    pub fn generate() -> Result<KeychainKey> {
        let label = ca_label()?;
        let _ = delete_by_label(&label);

        // We hand-build the attribute dictionary for `SecKeyCreateRandomKey` rather than going
        // through `GenerateKeyOptions::set_access_control(...)`. The reason: the only
        // access-control flag that conveys "non-extractable" is `kSecAccessControlPrivateKeyUsage`,
        // and that flag is Secure-Enclave-coupled — it triggers `errSecMissingEntitlement` on
        // unsigned `cargo`-built binaries. The raw `kSecAttrIsExtractable=false` attribute is
        // independent of SE entitlements and is what we actually want for a software key.
        let key = create_random_key(&label)
            .map_err(|e| anyhow!("generating Keychain key '{label}': {e}"))?;
        KeychainKey::from_seckey(key)
    }

    /// Look up the CA key by label. Returns `Ok(None)` when no entry matches (fresh machine, or
    /// the key was removed via Keychain Access).
    pub fn load() -> Result<Option<KeychainKey>> {
        let label = ca_label()?;
        let results = ItemSearchOptions::new()
            .key_class(KeyClass::private())
            .label(&label)
            .load_refs(true)
            .limit(Limit::Max(1))
            .search();
        let results = match results {
            Ok(r) => r,
            // The crate maps `errSecItemNotFound` to a generic Error — translating every
            // search-time error into "not found" would mask real failures, so we look for the
            // characteristic message and re-raise the rest.
            Err(e) if e.to_string().contains("specified item could not be found") => {
                return Ok(None);
            }
            Err(e) => return Err(anyhow!("searching keychain for CA key: {e}")),
        };
        for r in results {
            if let SearchResult::Ref(Reference::Key(k)) = r {
                return Ok(Some(KeychainKey::from_seckey(k)?));
            }
        }
        Ok(None)
    }

    /// Remove the CA key from the login keychain. Used by `install-ca --reset` and by the test
    /// suite during teardown. Returns `Ok(())` whether or not the entry existed.
    pub fn delete() -> Result<()> {
        let label = ca_label()?;
        let _ = delete_by_label(&label);
        Ok(())
    }

    fn delete_by_label(label: &str) -> Result<()> {
        // `SecKeyCreateRandomKey` writes BOTH the private and the public half into the
        // keychain. Filtering on `KeyClass::private()` here leaves the public half stranded,
        // visible in Keychain Access as an orphan "Adjacent local CA …" entry after a reset.
        // We delete each class explicitly. Ignore individual not-founds — at least one of the
        // two will be missing on the very first install.
        let private_res = ItemSearchOptions::new()
            .class(ItemClass::key())
            .key_class(KeyClass::private())
            .label(label)
            .limit(Limit::All)
            .delete();
        let public_res = ItemSearchOptions::new()
            .class(ItemClass::key())
            .key_class(KeyClass::public())
            .label(label)
            .limit(Limit::All)
            .delete();
        // Surface a meaningful error only if BOTH deletes failed for a reason other than
        // "nothing matched" — otherwise the common path (reset on a clean slate) would be
        // noisy.
        match (private_res, public_res) {
            (Ok(_), _) | (_, Ok(_)) => Ok(()),
            (Err(e), Err(_)) if e.to_string().contains("specified item could not be found") => {
                Ok(())
            }
            (Err(e), _) => Err(anyhow!("deleting existing Keychain key: {e}")),
        }
    }

    /// Generate an ECDSA P-256 key in the login keychain with a hand-built attribute
    /// dictionary. Mirrors what `GenerateKeyOptions::set_location(DefaultFileKeychain)` does,
    /// except the private-key sub-dictionary also carries `kSecAttrIsExtractable=false` — that
    /// flag is the entire reason for going through raw FFI. Returns the wrapped `SecKey` or the
    /// underlying `CFError` translated to a string for context.
    fn create_random_key(label: &str) -> std::result::Result<SecKey, String> {
        use core_foundation::error::CFError;
        let false_val = CFBoolean::false_value();
        let true_val = CFBoolean::true_value();
        // Private-key attrs: persisted, non-extractable. Public-key attrs: persisted (matches
        // what the high-level builder does so the public half is also in the keychain — gives
        // us SecKey::public_key()).
        let private_attrs: CFDictionary<CFType, CFType> = unsafe {
            CFDictionary::from_CFType_pairs(&[
                (
                    CFString::wrap_under_get_rule(kSecAttrIsPermanent).as_CFType(),
                    true_val.as_CFType(),
                ),
                (
                    CFString::wrap_under_get_rule(kSecAttrIsExtractable).as_CFType(),
                    false_val.as_CFType(),
                ),
            ])
        };
        let public_attrs: CFDictionary<CFType, CFType> = unsafe {
            CFDictionary::from_CFType_pairs(&[(
                CFString::wrap_under_get_rule(kSecAttrIsPermanent).as_CFType(),
                true_val.as_CFType(),
            )])
        };
        let size = CFNumber::from(256i32);
        let label_cf = CFString::new(label);
        let params: CFDictionary<CFType, CFType> = unsafe {
            CFDictionary::from_CFType_pairs(&[
                (
                    CFString::wrap_under_get_rule(kSecAttrKeyType).as_CFType(),
                    CFString::wrap_under_get_rule(kSecAttrKeyTypeECSECPrimeRandom).as_CFType(),
                ),
                (
                    CFString::wrap_under_get_rule(kSecAttrKeySizeInBits).as_CFType(),
                    size.as_CFType(),
                ),
                (
                    CFString::wrap_under_get_rule(kSecAttrLabel).as_CFType(),
                    label_cf.as_CFType(),
                ),
                (
                    CFString::wrap_under_get_rule(kSecPrivateKeyAttrs).as_CFType(),
                    private_attrs.as_CFType(),
                ),
                (
                    CFString::wrap_under_get_rule(kSecPublicKeyAttrs).as_CFType(),
                    public_attrs.as_CFType(),
                ),
            ])
        };
        let mut err: core_foundation_sys::error::CFErrorRef = std::ptr::null_mut();
        let key_ref: SecKeyRef =
            unsafe { SecKeyCreateRandomKey(params.as_concrete_TypeRef(), &mut err) };
        if key_ref.is_null() {
            let msg = if err.is_null() {
                "unknown SecKeyCreateRandomKey failure".to_string()
            } else {
                let wrapped = unsafe { CFError::wrap_under_create_rule(err) };
                format!("{wrapped}")
            };
            return Err(msg);
        }
        Ok(unsafe { SecKey::wrap_under_create_rule(key_ref) })
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;
    use anyhow::anyhow;

    /// Stub used on non-macOS builds. The HTTPS feature compiles, but anything that would touch
    /// the Keychain returns an explanatory error — the daemon's HTTPS task is best-effort and
    /// tolerates this by logging + skipping HTTPS startup.
    pub struct KeychainKey {
        _private: (),
    }

    impl KeychainKey {
        pub fn into_rcgen_keypair(self) -> Result<rcgen::KeyPair> {
            Err(unsupported())
        }

        pub fn sign_canary(&self) -> Result<()> {
            Err(unsupported())
        }
    }

    pub fn generate() -> Result<KeychainKey> {
        Err(unsupported())
    }

    pub fn load() -> Result<Option<KeychainKey>> {
        Err(unsupported())
    }

    pub fn delete() -> Result<()> {
        Ok(())
    }

    fn unsupported() -> anyhow::Error {
        anyhow!("Adjacent's local-CA Keychain backend requires macOS")
    }
}

pub use imp::*;
