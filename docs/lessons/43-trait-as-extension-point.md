<!-- Lesson for PR #43. Teaches one concept grounded in the real diff. -->

# PR #43 — Move CA private key into the macOS Keychain and add nameConstraints

> **Rust lesson:** Implementing a trait a *library* defines lets you plug your own behavior into that library's machinery — here, `KeychainKey` implements rcgen's `RemoteKeyPair` so certificate signing calls out to the macOS Keychain and the private key never enters process memory.
> **Tags:** `trait-impl` · `ffi`
> **Merged:** 2026-06-09 · +682/−38 · [View PR](https://github.com/nonrational/adjacent/pull/43)

## The situation

The local CA's private key used to sit at `~/.adjacent/ca.key` — a PEM file any backup tool
or process running as the user could copy. This PR moves the key into the macOS login keychain,
marked non-extractable, so the bytes never leave the OS. But rcgen — the crate that builds and
signs the CA cert — normally expects to *hold* the private key and sign with it directly. How do
you let rcgen produce a signed certificate when it can't have the key?

## The Rust idea

rcgen anticipated exactly this and left a seam: a trait, `RemoteKeyPair`. A library that exposes
a trait is saying "I don't need to own this behavior; give me anything that can do these three
things and I'll call back into it." That's a trait used as an **extension point**. You don't
subclass rcgen or fork it — you write a struct of your own and *implement its trait* for that
struct. rcgen then drives your code without ever knowing what's behind it: an HSM, a cloud KMS,
or in our case the macOS Keychain.

`RemoteKeyPair` requires three methods — hand me the public key, tell me the signature
algorithm, and sign these bytes. Fill all three and rcgen treats your type as a signer. The
`sign` method is where the magic happens: rcgen calls it, and *your* implementation decides how
the signature gets made. Ours forwards the request to the OS.

This is the mirror image of [PR #40](40-serve-https-local-ca.md). There, a
trait was a **bound** on a generic — "I accept any `S` that can read and write" — resolved at
compile time into a specialized copy per type (static dispatch). Here we go the other way: we
*implement* a trait and hand rcgen a `Box<dyn RemoteKeyPair>`, a **trait object**. rcgen holds
one boxed value and dispatches through a vtable at runtime, because it's compiled once and must
work with signers it's never heard of. Same feature — traits — two opposite uses: constraining a
caller vs. supplying a plug-in.

## In this PR

The trait implementation. rcgen defines the three method signatures; we fill each with
keychain-backed behavior:

```rust
// crates/adj/src/tls/keychain.rs
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
```

Note `sign` returns rcgen's `Result<Vec<u8>, RcgenError>`, not our own error type — the trait
dictates the shape, so we `map_err` the Keychain's error into the variant rcgen expects.

Handing the implementation to rcgen is one call. `Box::new(self)` puts our value on the heap as a
trait object so rcgen can store it behind `dyn RemoteKeyPair`:

```rust
// crates/adj/src/tls/keychain.rs
pub fn into_rcgen_keypair(self) -> Result<KeyPair> {
    KeyPair::from_remote(Box::new(self))
        .map_err(|e| anyhow!("constructing rcgen KeyPair from Keychain handle: {e}"))
}
```

From rcgen's side the key is now indistinguishable from any other. The cert-building code is
untouched — `self_signed` reaches through the trait and calls our `sign` when it needs a
signature:

```rust
// crates/adj/src/tls.rs — generate_ca()
let ca_handle = keychain::generate().context("creating Keychain CA key")?;
let ca_key = ca_handle.into_rcgen_keypair()?;
let params = build_ca_params();
let signed = params
    .self_signed(&ca_key)          // rcgen calls KeychainKey::sign() in here
    .context("self-signing CA cert")?;
```

### Supporting detail: the `unsafe` FFI seam

Our `sign` bottoms out in Apple's C Security framework, and generating the key does too. That
crossing is where `unsafe` shows up. A C constant the Rust bindings didn't expose gets linked
directly:

```rust
// crates/adj/src/tls/keychain.rs
extern "C" {
    static kSecAttrIsExtractable: core_foundation_sys::string::CFStringRef;
}
```

And the raw call into the framework is wrapped in `unsafe`:

```rust
// crates/adj/src/tls/keychain.rs — create_random_key()
let key_ref: SecKeyRef =
    unsafe { SecKeyCreateRandomKey(params.as_concrete_TypeRef(), &mut err) };
```

`unsafe` here doesn't mean "dangerous" — it means *the compiler can't verify this*. Rust's
guarantees (valid pointers, correct ownership, no data races) stop at the C boundary. The C
framework might hand back a null pointer, or expect a specific retain/release discipline the
borrow checker knows nothing about. `unsafe` is you signing off: "I've read the framework's
contract and I'm upholding it." It marks the exact lines where that promise lives, so a reader
knows precisely where Rust's safety net ends and manual care begins.

## Why it matters

The extension-point pattern is how you get library code to do something its authors never
imagined — sign with a key they can't see — without touching their source. rcgen stays a
dependency you pull from crates.io; the keychain integration lives entirely in your crate. The
trait is the contract between them, and the compiler enforces that you honor it: leave out
`sign`, or return the wrong error type, and it won't build.

A language without this seam would force a worse trade. You'd either fork rcgen to teach it about
keychains, or pull the private key into memory so rcgen could sign the ordinary way — which is
the exact thing this PR set out to prevent. The trait lets the key stay in the OS while rcgen
does its job none the wiser.

## Related lessons

- [PR #40](40-serve-https-local-ca.md) — the *other* face of traits: as a
  **bound** on a generic (static dispatch, monomorphized per type). Read side by side with this
  one to see constraining-a-caller vs. supplying-a-plug-in.

## Dig deeper

- [The Rust Book, ch. 10.2](https://doc.rust-lang.org/book/ch10-02-traits.html) — Traits: Defining Shared Behavior (implementing a trait on your own type; trait objects and `Box<dyn Trait>` are in ch. 18.2)
- [The Rustonomicon, ch. on FFI](https://doc.rust-lang.org/nomicon/ffi.html) — calling C, `extern` blocks, and why the boundary is `unsafe`
