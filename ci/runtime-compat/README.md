# Runtime-manager compatibility harness

Characterizes how `adj` resolves language-runtime version managers across two
launch contexts. See `docs/superpowers/specs/2026-06-28-runtime-manager-compatibility-design.md`.

## Run one cell locally

```bash
cargo build
export ADJ_BIN="$PWD/target/debug/adj"

cell=ci/runtime-compat/fixtures/mise-shim-python
eval "$("$cell/setup.sh" | grep -E '^(SHELL_PATH_ADD|LAUNCHD_EXTRA_PATH)=' | sed 's/^/export /')"

# inherited-shell context (shims on PATH)
PATH="$SHELL_PATH_ADD:$PATH" ci/runtime-compat/run-cell.sh "$cell" shell

# launchd-minimal context (bare PATH + optional LAUNCHD_EXTRA_PATH)
ci/runtime-compat/run-cell.sh "$cell" launchd
```

Each run prints a `RESULT ...` line and exits non-zero if the observed runtime
version diverges from the cell's documented expectation (`resolved` / `fallback`
/ `record`).
