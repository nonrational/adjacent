# Rust, one PR at a time

A teaching series that walks Adjacent's merged pull requests in order and pulls one
practical Rust lesson from each. The project is a real single-binary daemon — a
process supervisor, a reverse proxy, a local CA — so the concepts show up because
the code needed them, not because a tutorial contrived them.

One file per PR. Each teaches a single idea, grounded in code that PR actually
shipped (snippets are lifted from the real diff, not invented). Read them in number
order to watch the codebase — and the language features it reaches for — accrete.

## The honesty rule

Not every PR is about Rust. Some are the landing page, some are docs, one is a
ten-line deletion. Those are labeled plainly and teach whatever they *do* contain
(release engineering, project hygiene, doc-as-contract) instead of a fabricated Rust
concept. A teaching series that admits "no lesson here" is more trustworthy than one
that forces thirty-one. Non-Rust entries are marked ◦ below.

## The lessons

| PR | Lesson | The one-line takeaway |
|----|--------|-----------------------|
| [#16](16-supervised-app-with-logs.md) | `Arc<Mutex<T>>` + `tokio::spawn` | A detached task must *own* what it touches (`'static`); clone an `Arc` to share state, a `Mutex` to mutate it safely. |
| [#17](17-inject-port-into-child-env.md) | `Command` env injection | `Command` is a builder that inherits the parent env by default; `.env(k, v)` layers one variable on before `.spawn()`. |
| [#19](19-optional-fields-with-serde.md) | `Option<T>` + serde `Deserialize` | `Option<T>` is how a field says "may be absent"; serde fills a missing `Option` with `None` for you. |
| [#22](22-serialize-wire-dto-contract.md) | serde `Serialize` + wire DTOs | Emit public JSON through dedicated DTO types — derive when the shape matches, hand-write `impl Serialize` to pin an exact contract. |
| [#24](24-single-flight-boot-gate.md) | Single-flight with a per-key lock | Boot a resource once under concurrent demand: a short map lock hands out a per-name lock, held across the boot while the rest re-check. |
| [#29](29-deadlines-and-failure-enums.md) | Timeouts + custom error enums | Bound async work with `tokio::time::timeout` and a `deadline: Instant`; model *why* a wait ended as an enum. |
| [#34](34-fix-landing-page-redirect.md) ◦ | Client-side redirects | How a `<meta http-equiv="refresh">` redirect works, and why the `/` in `adj.ac/ent` is a load-bearing path boundary. |
| [#36](36-docs-as-config-contract.md) ◦ | Docs as the config contract | No Rust — but the ecosystem's answer to doc rot is doctests: `cargo test` runs the `rust` examples in your doc comments. |
| [#37](37-signals-bypass-drop-cleanup.md) | Signals bypass `Drop` | A signal-killed process skips destructors, so external cleanup (unlink a socket) must happen in a `tokio::signal` handler. |
| [#38](38-deterministic-async-tests.md) | Deterministic async tests | Assert on observable state you can count, never on timing — and a still-flaky test is a real race, not noise. |
| [#40](40-serve-https-local-ca.md) | Generics + trait bounds | One `serve_plain<S>` loop, monomorphized per stream type, drives both TCP and TLS at zero runtime cost. |
| [#41](41-raw-string-templated-doc.md) | Raw strings + `format!` | `r#"…"#` bakes a whole doc into the binary verbatim; `format!`'s `{name}` capture splices in the dynamic bits. |
| [#43](43-trait-as-extension-point.md) | Implementing a library trait (+ FFI) | Impl rcgen's `RemoteKeyPair` so signing calls out to the Keychain — the CA key never enters process memory. |
| [#45](45-iterator-map-collect-rows.md) | `iter().map().collect()` | Turn one collection into another with a lazy pipeline that does nothing until `collect` drives it. |
| [#46](46-revamp-landing-positioning-page.md) ◦ | Repo layout + dual licensing | The web frontend lives *outside* the Cargo `members` list so `cargo build` never sees it; `MIT OR Apache-2.0` is the ecosystem default. |
| [#47](47-integration-tests-separate-crate.md) | Integration test layout | Each file in `tests/` is its own crate touching only the public API; a binary-only crate drives itself via `CARGO_BIN_EXE_adj`. |
| [#48](48-warn-dont-swallow-errors.md) | Don't swallow the `Err` | `.unwrap_or(default)` throws the error away; `.unwrap_or_else(\|err\| …)` hands it to you to surface first. |
| [#49](49-parse-dont-validate-actionable-errors.md) | Parse, don't validate | Push the check into the parser so a bad value can't exist downstream — and spend a line making the `Err` tell the user what to do. |
| [#50](50-weak-refs-self-cleaning-map.md) | `Weak` refs + `HashMap::retain` | Store `Weak` values so a map garbage-collects itself; `retain` sweeps the dead entries in one in-place pass. |
| [#51](51-typed-http-header-values.md) | Typed headers as newtypes | A `HeaderValue` is a validated byte-string, not a `String`; fallible construction is how the type blocks header injection. |
| [#52](52-abort-detached-tokio-tasks.md) | Task cancellation | Dropping a `JoinHandle` *detaches* the task; hold it and `.abort()` — ideally from a `Drop` guard so it fires on every path. |
| [#53](53-http-upgrade-stream-splicing.md) | HTTP upgrades + `copy_bidirectional` | An `Upgrade` trades HTTP framing for the raw stream; claim it with `hyper::upgrade::on`, then splice both halves. |
| [#55](55-races-beyond-the-borrow-checker.md) | Races the borrow checker can't catch | `Send`/`Sync` kill in-process *data* races; a TOCTOU race over a port or file is still entirely on you. |
| [#56](56-split-once-structured-keys.md) | `str::split_once` | Cut a string at the first delimiter into two borrowed `&str` slices, returning `Option` so "no delimiter" is a case, not a bug. |
| [#60](60-compile-time-version-stamping.md) | `build.rs` + `env!`/`option_env!` | A build script emits `cargo:rustc-env=…` before compile; `env!`/`option_env!` bake those into the binary as `&'static str`. |
| [#63](63-what-clippy-teaches.md) | What clippy teaches | Each lint names an idiom or a footgun; the honest reply is adopt it or `#[allow]` it *with a reason*. |
| [#65](65-health-files-and-metadata.md) ◦ | OSS hygiene + crates.io metadata | No Rust — the crates.io analog of `.github/` health files is `Cargo.toml` package metadata (`keywords`, `categories`, `rust-version`). |
| [#68](68-lean-agent-docs.md) ◦ | Sometimes there's no lesson | A ten-line docs deletion. The honest move is to say so — that's the series' principle in action. |
| [#69](69-distributing-a-rust-cli.md) ◦ | Distributing a Rust CLI | Homebrew tap (prebuilt, no toolchain) vs `cargo install` (compiles to `~/.cargo/bin`) vs a raw tarball — who eats the complexity. |
| [#75](75-hand-render-config-to-disk.md) | Generating a config file | Hand-render human-editable TOML so comments survive; write with `std::fs::write` behind a `!path.exists()` guard so you never clobber. |
| [#76](76-derived-env-precedence-order.md) | Derived data + precedence order | Compute a namespace with `format!` into a `Vec<(k, v)>`; inject it last so `Command::env`'s last-writer-wins makes it win. |

◦ = little or no Rust; labeled honestly and teaching what the PR does contain.

## Reading paths by theme

If you'd rather follow a thread than go in order:

- **Shared state & async concurrency** — [#16](16-supervised-app-with-logs.md) → [#24](24-single-flight-boot-gate.md) → [#50](50-weak-refs-self-cleaning-map.md) → [#52](52-abort-detached-tokio-tasks.md) → [#55](55-races-beyond-the-borrow-checker.md)
- **Error handling** — [#29](29-deadlines-and-failure-enums.md) → [#48](48-warn-dont-swallow-errors.md) → [#49](49-parse-dont-validate-actionable-errors.md)
- **Traits & generics** — [#40](40-serve-https-local-ca.md) (bounds / static dispatch) → [#43](43-trait-as-extension-point.md) (impl / dynamic dispatch)
- **Serde, config & data** — [#19](19-optional-fields-with-serde.md) → [#22](22-serialize-wire-dto-contract.md) → [#75](75-hand-render-config-to-disk.md) → [#76](76-derived-env-precedence-order.md)
- **The proxy stack** — [#24](24-single-flight-boot-gate.md) → [#40](40-serve-https-local-ca.md) → [#51](51-typed-http-header-values.md) → [#53](53-http-upgrade-stream-splicing.md)
- **Process lifecycle & env** — [#17](17-inject-port-into-child-env.md) → [#37](37-signals-bypass-drop-cleanup.md) → [#76](76-derived-env-precedence-order.md)
- **Strings & slices** — [#41](41-raw-string-templated-doc.md) → [#45](45-iterator-map-collect-rows.md) → [#56](56-split-once-structured-keys.md)
- **Testing** — [#38](38-deterministic-async-tests.md) → [#47](47-integration-tests-separate-crate.md) → [#55](55-races-beyond-the-borrow-checker.md)
- **Tooling, ecosystem & distribution** — [#60](60-compile-time-version-stamping.md) → [#63](63-what-clippy-teaches.md) → [#65](65-health-files-and-metadata.md) → [#69](69-distributing-a-rust-cli.md)

## Adding a lesson for a new PR

1. Copy [`_TEMPLATE.md`](_TEMPLATE.md) to `docs/lessons/<PR-number>-<slug>.md` (slug = 3–5 kebab-case words from the title).
2. Read the real diff (`gh pr diff <N>`) and pick the *single* most instructive concept it demonstrates. Verify the angle against the code — the diff wins over any preconception.
3. Use real snippets from that diff, with the file path. Never invent code.
4. If the PR has little or no Rust, say so plainly and teach what it does contain.
5. Add a row to the table above (keep it in PR-number order) and, where it fits, a step on a themed reading path.

### Where this is headed

The intent is a canonical pattern: when a PR merges, its teaching plan gets posted as a
comment on the PR itself, so the lesson lives next to the change that motivated it. These
files are written to double as that comment — self-contained, and postable as-is with
`gh pr comment <N> --body-file docs/lessons/<N>-<slug>.md`.
