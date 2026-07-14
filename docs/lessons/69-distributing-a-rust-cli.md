<!-- Lesson for PR #69. Non-Rust diff (homepage), teaches Rust-CLI install/distribution. -->

# PR #69 — Add alpha install directions to the homepage

> **Rust lesson:** No Rust in this diff (it's the `ent/` homepage plus a Homebrew formula rename) — but the *content* teaches the consumer side of shipping a Rust CLI: a Homebrew tap gives users a prebuilt binary with no toolchain, versus `cargo install` compiling from source into `~/.cargo/bin`, versus grabbing the release tarball by hand.
> **Tags:** `distribution` · `homebrew` · `cargo-install`
> **Merged:** 2026-06-21 · +46/−11 · [View PR](https://github.com/nonrational/adjacent/pull/69)

## The situation

The alpha was installable but the homepage still said "coming soon." This PR put
the real install steps on the page and renamed the Homebrew package `adj → adjacent`
so `brew install adjacent` reads naturally. The diff is HTML, bash, YAML, a Ruby
formula, and Markdown — zero Rust. Worth teaching anyway, because "how does a
stranger get your Rust binary onto their machine" is a real question every CLI
project answers, and this PR answers it in public.

## The idea (no Rust this time)

A compiled Rust CLI is just a binary. Getting it onto a user's `PATH` has three
common routes, and they trade off differently:

1. **A package manager (Homebrew tap).** The user runs two commands, gets a
   prebuilt binary, and never needs a Rust toolchain. Updates flow through
   `brew upgrade`. You (the producer) do the compiling once in CI and publish the
   artifact.
2. **`cargo install`.** Compiles from source on the user's machine into
   `~/.cargo/bin`. Requires them to have Rust installed, and the first build can
   take minutes. Updates are `cargo install --force`. Great for Rust developers,
   a non-starter for everyone else.
3. **The prebuilt release tarball, by hand.** Download, extract, drop the binary
   on `PATH`. No toolchain, no Homebrew — but you own every update yourself.

The Homebrew route is really #3 with a manager wrapped around it: the formula
just points at the same release tarball and automates the download-and-place.

## In this PR

The homepage now renders the two-command install path (`ent/index.html`):

```html
<!-- ent/index.html -->
<section>
  <div class="label">install &middot; alpha</div>
  <div class="cmds">
    <code class="cmd"><span class="prompt">$</span>brew tap nonrational/adjacent https://github.com/nonrational/adjacent<span class="note"># straight from the repo &mdash; no separate tap</span></code>
    <code class="cmd"><span class="prompt">$</span>brew install adjacent<span class="note"># Apple Silicon, unsigned alpha build</span></code>
  </div>
</section>
```

Which the user runs as:

```sh
brew tap nonrational/adjacent https://github.com/nonrational/adjacent
brew install adjacent
```

Two subtleties worth naming:

**The package name and the binary name don't have to match.** `brew install
adjacent` installs a CLI you invoke as `adj`. Homebrew's formula name is just a
handle for the package; what lands on `PATH` is whatever the formula installs.
This PR renamed the *package* to `adjacent` while the CLAUDE.md contract keeps the
*binary* `adj`.

**What the tap actually wraps is route #3.** The formula's source of truth points
straight at a prebuilt release artifact (`.github/scripts/render-formula.sh`):

```bash
# .github/scripts/render-formula.sh
url="https://github.com/${repo}/releases/download/${tag}/adj-${version}-aarch64-apple-darwin.tar.gz"
```

So the user who runs `brew install adjacent` and the user who downloads that
`.tar.gz` by hand get byte-for-byte the same binary. Homebrew is the convenience
layer, not a different build.

For contrast, the from-source route lives in this repo's `justfile`:

```make
# justfile
install:
  cargo install --path crates/adj
```

That compiles the workspace and installs `adj` into `~/.cargo/bin` — the Rust
developer's path, needing a full toolchain and a real compile, no prebuilt
artifact involved.

## Why it matters

Pick the wrong default and you lose users. Lead a non-Rust audience with `cargo
install` and you've told them to install a compiler and wait for a cold build
before they can try your tool — most bounce. Lead with a Homebrew tap and the
cost is on *you*: CI has to build, sign (or admit it's unsigned, as this alpha
does), and publish the artifact, and you have to keep the formula's `url` and
`sha256` in sync with each release. This PR even adds a `smoke` CI job that taps
and installs from the *published* formula, so a broken install path fails the
release instead of a user. The producer eats the complexity so the consumer's
path stays two commands.

## Related lessons

- **PR #60** built the *producer* half this consumes — the release CI, the
  in-repo tap formula, and version stamping that make `brew install adjacent`
  resolve to a real artifact:
  [View PR #60](https://github.com/nonrational/adjacent/pull/60). This PR is the
  consumer-facing directions for that machinery.
- [PR #46](46-revamp-landing-positioning-page.md) — the other side of the same
  `ent/` homepage: how a single-binary Rust project carries a non-Rust web
  frontend beside its Cargo workspace. Same "honest, no-Rust diff" shape as this
  one.

## Dig deeper

- [The Cargo Book — `cargo install`](https://doc.rust-lang.org/cargo/commands/cargo-install.html) — installs a binary from source into `~/.cargo/bin`; the `--path`, `--force`, and `--root` flags that control where it builds from and lands.
- [Homebrew — How to Create and Maintain a Tap](https://docs.brew.sh/How-to-Create-and-Maintain-a-Tap) — the `user/tap/formula` naming triplet and why the tap and formula sharing a name (`nonrational/adjacent` + `adjacent`) is fine.
