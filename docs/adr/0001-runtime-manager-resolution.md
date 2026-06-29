# 1. Fail loudly on runtime-version mismatch; document the exec-wrapped pattern; never boot via a login shell

**Status:** Accepted (2026-06-29)\
**Deciders:** nonrational (human), nonreagent (agent)\
**Related:** PR #77; `docs/superpowers/specs/2026-06-28-runtime-manager-compatibility-design.md`

## Context

`adj` boots apps with `sh -c <cmd>`, working directory set to the app dir, inheriting the daemon's environment. The empirical matrix in PR #77 (rbenv, asdf, mise, uv, nvm across an inherited-shell PATH and a bare launchd PATH) established the known behavior:

- **Shim managers** (rbenv, asdf, mise-shim) resolve the pinned runtime **only if their shims are on the daemon's inherited PATH**. Under a launchd-minimal PATH they silently fall back to the system toolchain (system Ruby/Python booted the app with the *wrong* version), or — for Node, which has no `/usr/bin/node` — fail to boot at all (`exit 127`).
- **Activation-hook managers** (`mise activate`, nvm) never resolve under `sh -c`: their `cd`/PWD hooks only fire in an interactive shell. They get the wrong version in every context.
- **Exec-wrapped invocations** (`mise exec`, `mise run`, `uv run`) **do** resolve the pin — even under a bare launchd PATH — as long as the manager binary itself is reachable.

Two properties of the failure matter more than the per-manager details:

1. **The common failure is silent.** A shim manager under launchd boots the app with the system runtime; it serves 200s and looks healthy. Nothing signals the version is wrong. "Works but wrong" is the worst failure mode for a tool whose value is a trustworthy shared environment — and the agent half of that shared environment will not read a log line.
2. **It is launch-context-dependent, and the intended deployment is the failing one.** The same app resolves correctly when `adj` is launched from a configured terminal but falls back under launchd — and launchd is the recommended always-on deployment.

Underneath both is an architectural mismatch: **a single long-lived daemon inherits one environment, but modern version managers resolve per-directory.** One launch context cannot carry correct per-directory resolution for N registered apps. This is a design tension, not a patchable bug.

`adj`'s design value is **declarative, loud, and proud** — no zero-config magic that silently does the wrong thing, no invisible state.

## Decision

1. **Fail loudly on version mismatch (adopt).** The pin file *is* the declaration. At boot, `adj` pre-flight-probes the runtime it would actually use (resolves the interpreter in the exact environment it will boot the app, asks its version), compares it to the declared pin, and **refuses to boot on a mismatch** — surfacing the conflict through the same loud path as a boot crash (`adj status` + the app log), not a soft warning that scrolls past. No pin file means no declaration means no opinion: boot as-is. The error names the remedy (wrap the cmd in `mise exec` / `mise run`, or put the manager's shims on the daemon's PATH).

2. **Document the exec-wrapped pattern (adopt).** `mise run <task>` / `mise exec -- <cmd>` / `uv run` are the supported way to get pinned runtimes, verified to work even under a bare launchd PATH. No `adj` code; the failure message from (1) points here.

3. **Never boot via a login shell, and no environment-variable override (adopt).** See prior art below. Any future opt-out lives in `adjacent.toml` (committed, visible, declarative) — never an env var.

4. **Defer per-manager adapters (do not adopt now).** Transparent resolution via language-specific adapters cuts against the stack-agnostic ethos; revisit only if the fail-loud experience proves too coarse.

## Options considered

- **Login-shell execution** (`$SHELL -lic` instead of `sh -c`). **Prior art: Puma-dev did exactly this** (ran the app under a login bash shell). If your interactive shell was zsh, the environment never activated correctly; because it was zero-config there was no good way to override it, and reaching for environment variables only added more invisible state. Rejected as both default and fallback — it reproduces a known trap and violates the declarative value.
- **Soft warning** (boot anyway, log a mismatch). Rejected: a warning is still semi-silent. Humans miss it and agents do not read logs, so it fails to eliminate the running-the-wrong-version outcome. Fail-closed does.
- **Per-manager adapters.** Deferred (see Decision 4).
- **Do nothing.** Rejected — leaves the silent wrong-version property in place.

## Consequences

- **+** Silent wrong-version is eliminated, not merely surfaced — the daemon fails closed.
- **+** One source of truth: the committed pin file. The remedy travels with the error.
- **−** A stale or unwanted committed pin file now blocks boot until it is fixed or removed. Intended: a declaration you did not mean is still a declaration.
- **−** `mise activate` / nvm users get a hard error until they adopt the exec pattern or put shims on the daemon's PATH — louder than a warning, by design.
- **−** `adj` must pre-flight probe: map a pin-file type to its interpreter and ask the booted runtime its version. This is the first manager-specific knowledge in `adj`; scope is bounded to pin-file *filenames* plus a version probe — no resolution logic.
- **=** Node under launchd already fails to boot; Ruby/Python mismatch now fails the same way — consistent behavior across ecosystems.

## Open follow-ups

- Where the conflict surfaces: a field on the status DTO, the boot path's error (502 with reason), the app log — likely all three. Own spec.
- The pin-file → resolved-version probe mechanism. Own spec.
