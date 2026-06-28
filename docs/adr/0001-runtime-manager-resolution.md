# 1. Surface runtime-version mismatches; document the exec-wrapped pattern; defer transparent resolution

**Status:** Proposed (2026-06-28) — awaiting human decision\
**Deciders:** nonrational (human), nonreagent (agent)\
**Related:** PR #77; `docs/superpowers/specs/2026-06-28-runtime-manager-compatibility-design.md`

## Context

`adj` boots apps with `sh -c <cmd>`, working directory set to the app dir, inheriting the daemon's environment. The empirical matrix in PR #77 (rbenv, asdf, mise, uv, nvm across an inherited-shell PATH and a bare launchd PATH) established the known behavior:

- **Shim managers** (rbenv, asdf, mise-shim) resolve the pinned runtime **only if their shims are on the daemon's inherited PATH**. Under a launchd-minimal PATH they silently fall back to the system toolchain (system Ruby/Python booted the app with the *wrong* version), or — for Node, which has no `/usr/bin/node` — fail to boot at all (`exit 127`).
- **Activation-hook managers** (`mise activate`, nvm) never resolve under `sh -c`: their `cd`/PWD hooks only fire in an interactive shell. They get the wrong version in every context.
- **Exec-wrapped invocations** (`mise exec`, `mise run`, `uv run`) **do** resolve the pin — even under a bare launchd PATH — as long as the manager binary itself is reachable.

Two properties of the failure matter more than the per-manager details:

1. **The common failure is silent.** A shim manager under launchd boots the app with the system runtime; it serves 200s and looks healthy. Nothing signals the version is wrong. "Works but wrong" is the worst failure mode for a tool whose value is a trustworthy shared environment.
2. **It is launch-context-dependent, and the intended deployment is the failing one.** The same app resolves correctly when `adj` is launched from a configured terminal but falls back under launchd — and launchd is the recommended always-on deployment.

Underneath both is an architectural mismatch: **a single long-lived daemon inherits one environment, but modern version managers resolve per-directory.** One launch context cannot carry correct per-directory resolution for N registered apps. This is a design tension, not a patchable bug.

## Decision

Three parts:

1. **Detect and warn (adopt).** At boot, if the app dir contains a recognized pin file (`.tool-versions`, `.mise.toml`, `.ruby-version`, `.python-version`, `.nvmrc`) and the resolved interpreter does not match the pin, surface the mismatch in `adj status` and the app log. This does **not** change resolution; it converts silent-wrong into visible-wrong. It fits the supervisor's existing env-layering and stays stack-agnostic (filename recognition + a version probe, not per-language resolution logic).

2. **Document the exec-wrapped pattern (adopt).** Recommend `mise run <task>` / `mise exec -- <cmd>` / `uv run` as the supported way to get pinned runtimes, especially under launchd. This is verified to work and requires no `adj` code.

3. **Defer transparent resolution (do not adopt now).** Neither per-manager adapters nor login-shell execution is adopted. Each trades away a core `adj` value; see options below.

## Options considered

- **Login-shell execution** (`$SHELL -lic` instead of `sh -c`): matches the naive user expectation and would make `mise activate` / nvm / direnv "just work." Rejected as the default — it sources arbitrary rc files on every boot (slow, non-deterministic, can hang or prompt), which breaks the supervisor's deterministic, non-interactive model.
- **Per-manager adapters** (`adj` detects mise/asdf and wraps the cmd): convenient, but a set of language-specific adapters cuts against the stack-agnostic ethos and is brittle as managers change. Deferred, not rejected — could revisit if detect-and-warn shows the friction is high.
- **Do nothing:** leaves the silent wrong-version property in place. Rejected.

## Consequences

- **+** The dangerous silent-wrong case becomes visible; cheap and on-ethos.
- **+** Users get a supported pattern (`mise run` / `mise exec` / `uv run`) they can adopt today.
- **−** Users on `mise activate` / nvm must change their cmd (or put shims on the daemon's PATH) to get pinned runtimes; `adj` will not transparently fix their setup.
- **−** Detect-and-warn is the first place `adj` encodes any manager-specific knowledge. Scope is bounded to pin-file *filenames* plus a version probe — no resolution logic — to keep that footprint minimal.
- **−** Node under launchd stays unbootable unless `node` is on PATH or the cmd is exec-wrapped; documented as expected, not silently degraded.

## Open follow-ups

- Where the warning surfaces: a field on the status DTO, a boot-time log line, or both.
- The pin-file → resolved-version probe mechanism (how `adj` reads the pinned version and asks the booted runtime for its actual version) — its own spec.
