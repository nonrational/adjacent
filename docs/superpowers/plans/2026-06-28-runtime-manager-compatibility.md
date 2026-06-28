# Runtime-manager Compatibility Characterization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a CI harness that empirically characterizes how `adj` resolves language-runtime version managers (rbenv, asdf, mise, uv, nvm) across two launch contexts, using a non-default-pin signal so silent system-toolchain fallback fails loudly.

**Architecture:** A POSIX-shell harness (`ci/runtime-compat/`) starts a real `adj` daemon under a chosen environment context, registers a fixture app whose health response echoes its own resolved runtime version, then asserts that version against the fixture's pin (or against an explicit "fallback" expectation). One fixture directory per matrix cell carries the manager's pin file, an `adjacent.toml`, and a `setup.sh` that installs the manager. A GitHub Actions workflow fans the fixtures × {inherited-shell, launchd-minimal} contexts over `ubuntu-latest`, plus one `macos-14` launchd smoke job.

**Tech Stack:** Bash, `curl`, the existing `adj` binary (`cargo build` → `target/debug/adj`), GitHub Actions; managers rbenv, asdf, mise, uv, nvm.

**Spec:** `docs/superpowers/specs/2026-06-28-runtime-manager-compatibility-design.md`

---

## Key mechanics (read before Task 1)

- **Daemon boot:** `adj daemon` runs in the foreground; it lazy-boots apps on first proxied request. The harness backgrounds it.
- **Port discovery:** start the daemon with `ADJACENT_PROXY_PORT=0`; it writes the kernel-assigned port to `$ADJACENT_HOME/proxy.port`. The harness reads that file — never hard-codes 8080.
- **Routing:** the proxy routes by `Host: <name>.adj.ac`. Every fixture names its app `app`, so requests go to `app.adj.ac`. Each cell×context run uses a fresh `ADJACENT_HOME` (so the registry starts empty and `app` never clashes).
- **Working directory:** `adj` runs the app cmd with `current_dir` = the registered app dir (the fixture dir), which is where every manager reads its pin file (`.ruby-version`, `.tool-versions`, `.mise.toml`, `.python-version`, `.nvmrc`).
- **The two contexts** (realized in `run-cell.sh`):
  - `shell` — the daemon inherits the caller's `PATH` (the cell's `setup.sh` has added the manager's shims/bins). Models launching `adj` from a configured terminal.
  - `launchd` — the daemon starts under `env -i` with `PATH=/usr/bin:/bin:/usr/sbin:/sbin` plus the fixture's optional `LAUNCHD_EXTRA_PATH` (the dir holding a self-resolving binary like `mise`/`uv`). Models the launchd-started always-on daemon.
- **Expectations** (`cell.env` per fixture): each context has an expectation the harness asserts:
  - `resolved` → observed version MUST equal the pin.
  - `fallback` → observed version MUST NOT equal the pin (a system/global toolchain stood in).
  - `record` → unknown; always pass, just log observed (used for the ❓ `mise exec`/`mise run`/`uv` launchd cells until we have data).
- **HTTPS:** set `ADJACENT_HTTPS_PORT=0`; with no CA installed the HTTPS task logs an error and exits while HTTP keeps serving — exactly what we want, no CA setup needed.

---

## File structure

```
ci/runtime-compat/
  lib.sh                       # daemon start/stop, readiness, request, assert helpers
  run-cell.sh                  # entrypoint: run-cell.sh <fixture-dir> <shell|launchd>
  servers/
    server.rb                  # echoes "RUBY <version>"
    server.js                  # echoes "NODE <version>"
    server.py                  # echoes "PYTHON <version>"
  fixtures/
    mise-shim-python/          # tracer (Task 2)
    rbenv-ruby/                # Task 3
    asdf-node/                 # Task 4
    mise-activate-ruby/        # Task 5  (Berkopec failure)
    mise-exec-node/            # Task 6
    mise-run-ruby/             # Task 7  (Berkopec remedy)
    uv-python/                 # Task 8
    nvm-node/                  # Task 9
  README.md                    # how to run a cell locally (Task 10)
.github/workflows/
  runtime-compat.yml           # ubuntu matrix + aggregate + macOS smoke (Tasks 11-12)
```

Each `fixtures/<cell>/` contains: the manager's pin file, `adjacent.toml`, `cell.env`, and `setup.sh`.

---

## Task 1: Harness core — servers, lib, runner

**Files:**
- Create: `ci/runtime-compat/servers/server.rb`
- Create: `ci/runtime-compat/servers/server.js`
- Create: `ci/runtime-compat/servers/server.py`
- Create: `ci/runtime-compat/lib.sh`
- Create: `ci/runtime-compat/run-cell.sh`

- [ ] **Step 1: Write the three version-echo servers**

Each binds `$PORT` and returns its own interpreter version. The version comes from the *running* interpreter, so the response proves which runtime the manager selected.

`ci/runtime-compat/servers/server.rb`:

```ruby
require "socket"

port = Integer(ENV.fetch("PORT"))
body = "RUBY #{RUBY_VERSION}\n"
server = TCPServer.new("127.0.0.1", port)
loop do
  conn = server.accept
  conn.gets                                   # request line
  while (line = conn.gets) && line != "\r\n"; end  # drain headers
  conn.write "HTTP/1.1 200 OK\r\nContent-Length: #{body.bytesize}\r\nConnection: close\r\n\r\n#{body}"
  conn.close
end
```

`ci/runtime-compat/servers/server.js`:

```javascript
const http = require("http");

const port = parseInt(process.env.PORT, 10);
const body = `NODE ${process.versions.node}\n`;
http
  .createServer((_req, res) => {
    res.writeHead(200, { "Content-Length": Buffer.byteLength(body) });
    res.end(body);
  })
  .listen(port, "127.0.0.1");
```

`ci/runtime-compat/servers/server.py`:

```python
import os
import platform
from http.server import BaseHTTPRequestHandler, HTTPServer

PORT = int(os.environ["PORT"])
BODY = f"PYTHON {platform.python_version()}\n".encode()


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Length", str(len(BODY)))
        self.end_headers()
        self.wfile.write(BODY)

    def log_message(self, *args):
        pass


HTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
```

- [ ] **Step 2: Write `lib.sh` (daemon + request + assert helpers)**

`ci/runtime-compat/lib.sh`:

```bash
# shellcheck shell=bash
# Helpers for the runtime-manager compatibility harness.
# Requires: ADJ_BIN (absolute path to the adj binary).

set -uo pipefail

start_daemon() {
  # $1 = context: "shell" | "launchd"
  # $2 = LAUNCHD_EXTRA_PATH (may be empty)
  local context="$1" extra="$2"
  ADJACENT_HOME="$(mktemp -d)"
  export ADJACENT_HOME
  rm -f "$ADJACENT_HOME/proxy.port"

  if [ "$context" = "launchd" ]; then
    local minimal="/usr/bin:/bin:/usr/sbin:/sbin"
    [ -n "$extra" ] && minimal="${minimal}:${extra}"
    env -i \
      HOME="$HOME" \
      ADJACENT_HOME="$ADJACENT_HOME" \
      ADJACENT_PROXY_PORT=0 \
      ADJACENT_HTTPS_PORT=0 \
      PATH="$minimal" \
      "$ADJ_BIN" daemon &
  else
    ADJACENT_PROXY_PORT=0 ADJACENT_HTTPS_PORT=0 "$ADJ_BIN" daemon &
  fi
  DAEMON_PID=$!

  # Wait for the daemon to publish its proxy port (race-free discovery).
  local tries=0
  while [ ! -s "$ADJACENT_HOME/proxy.port" ]; do
    tries=$((tries + 1))
    if [ "$tries" -gt 100 ]; then
      echo "daemon never wrote proxy.port" >&2
      return 1
    fi
    sleep 0.1
  done
  PROXY_PORT="$(cat "$ADJACENT_HOME/proxy.port")"
}

stop_daemon() {
  # Preserve the script's pending exit status: this runs from an EXIT trap, and
  # bash would otherwise make the trap's last command (wait on the SIGTERMed
  # daemon, status 143) the script's exit code — masking pass(0)/fail(1).
  local status=$?
  [ -n "${DAEMON_PID:-}" ] && kill "$DAEMON_PID" 2>/dev/null
  # || true: set -e is active in the EXIT trap; without it, wait's 143 exit
  # status would abort the function before return "$status" executes.
  wait "$DAEMON_PID" 2>/dev/null || true
  [ -n "${ADJACENT_HOME:-}" ] && rm -rf "$ADJACENT_HOME"
  return "$status"
}

# Request app.adj.ac through the proxy; first hit lazy-boots the app.
fetch_version() {
  curl -fsS --max-time 90 -H "Host: app.adj.ac" "http://127.0.0.1:${PROXY_PORT}/"
}

# assert_expectation <expect> <observed> <pin> -> sets RESULT_STATUS
assert_expectation() {
  local expect="$1" observed="$2" pin="$3"
  case "$expect" in
    resolved)
      [ "$observed" = "$pin" ] && RESULT_STATUS=pass || RESULT_STATUS=fail ;;
    fallback)
      [ "$observed" != "$pin" ] && RESULT_STATUS=pass || RESULT_STATUS=fail ;;
    record)
      RESULT_STATUS=pass ;;
    *)
      echo "unknown expectation: $expect" >&2; RESULT_STATUS=fail ;;
  esac
}
```

- [ ] **Step 3: Write `run-cell.sh` (entrypoint)**

`ci/runtime-compat/run-cell.sh`:

```bash
#!/usr/bin/env bash
# Run one matrix cell in one context and print a RESULT line.
# Usage: run-cell.sh <fixture-dir> <shell|launchd>
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=lib.sh
source "$HERE/lib.sh"

FIXTURE="$(cd "$1" && pwd)"
CONTEXT="$2"
: "${ADJ_BIN:?set ADJ_BIN to the adj binary path}"

# shellcheck source=/dev/null
source "$FIXTURE/cell.env"   # MANAGER RUNTIME PIN EXPECT_SHELL EXPECT_LAUNCHD LAUNCHD_EXTRA_PATH(optional)

if [ "$CONTEXT" = "shell" ]; then
  EXPECT="$EXPECT_SHELL"; EXTRA=""
else
  EXPECT="$EXPECT_LAUNCHD"; EXTRA="${LAUNCHD_EXTRA_PATH:-}"
fi

trap stop_daemon EXIT
start_daemon "$CONTEXT" "$EXTRA"
"$ADJ_BIN" add "$FIXTURE" >/dev/null

OBSERVED_RAW="$(fetch_version || echo "BOOT_FAILED")"
OBSERVED="$(printf '%s' "$OBSERVED_RAW" | awk '{print $2}')"   # second field = version

# A failed boot must never satisfy a `fallback` (or `record`) expectation — that
# would be a false green. `fallback` means "booted with the wrong runtime", not
# "never booted". A missing version is a hard failure regardless of expectation.
if [ "$OBSERVED_RAW" = "BOOT_FAILED" ] || [ -z "$OBSERVED" ]; then
  echo "RESULT manager=$MANAGER context=$CONTEXT pin=$PIN observed=none expect=$EXPECT status=fail"
  exit 1
fi

assert_expectation "$EXPECT" "$OBSERVED" "$PIN"
echo "RESULT manager=$MANAGER context=$CONTEXT pin=$PIN observed=$OBSERVED expect=$EXPECT status=$RESULT_STATUS"
[ "$RESULT_STATUS" = "pass" ]
```

- [ ] **Step 4: Make the runner executable and sanity-check syntax**

Run:
```bash
chmod +x ci/runtime-compat/run-cell.sh
bash -n ci/runtime-compat/run-cell.sh && bash -n ci/runtime-compat/lib.sh && echo "syntax OK"
```
Expected: `syntax OK`

- [ ] **Step 5: Commit**

```bash
git add ci/runtime-compat/servers ci/runtime-compat/lib.sh ci/runtime-compat/run-cell.sh
git commit ci/runtime-compat/servers ci/runtime-compat/lib.sh ci/runtime-compat/run-cell.sh \
  -m "Add runtime-compat harness core: version-echo servers and cell runner"
```

(Use the project's agent-identity commit flags from CLAUDE.md.)

---

## Task 2: Tracer cell — mise shim, Python (locally verifiable)

This is the end-to-end tracer: mise installs fast, so you can run both contexts on your machine and confirm the harness distinguishes `resolved` from `fallback`.

**Files:**
- Create: `ci/runtime-compat/fixtures/mise-shim-python/.mise.toml`
- Create: `ci/runtime-compat/fixtures/mise-shim-python/adjacent.toml`
- Create: `ci/runtime-compat/fixtures/mise-shim-python/cell.env`
- Create: `ci/runtime-compat/fixtures/mise-shim-python/setup.sh`

- [ ] **Step 1: Write the fixture files**

`.mise.toml`:
```toml
[tools]
python = "3.11.9"
```

`adjacent.toml`:
```toml
name = "app"
cmd = "python ../../servers/server.py"
boot_timeout = 120
```

`cell.env`:
```bash
MANAGER=mise-shim
RUNTIME=python
PIN=3.11.9
EXPECT_SHELL=resolved
EXPECT_LAUNCHD=fallback
```

`setup.sh` (installs mise + the pinned Python, exports the shim dir for the shell context):
```bash
#!/usr/bin/env bash
# Install mise + pinned python via shims. Echoes PATH additions for the shell context.
set -euo pipefail
# mise's python-build-standalone attestation verification fails in clean CI
# environments (no GitHub OIDC token / network path to attestation endpoint).
export MISE_PYTHON_GITHUB_ATTESTATIONS=false
HERE="$(cd "$(dirname "$0")" && pwd)"

if ! command -v mise >/dev/null 2>&1; then
  curl -fsSL https://mise.run | sh
fi
export PATH="$HOME/.local/bin:$PATH"
mise install python@3.11.9
mise reshim

# The shim dir on PATH is mise's non-activation resolution path.
echo "SHELL_PATH_ADD=$HOME/.local/share/mise/shims:$HOME/.local/bin"
# launchd context: nothing extra — a bare PATH must fall back.
echo "LAUNCHD_EXTRA_PATH="
```

Make it executable:
```bash
chmod +x ci/runtime-compat/fixtures/mise-shim-python/setup.sh
```

- [ ] **Step 2: Build adj and run the cell in `shell` context — verify RESOLVED**

Run:
```bash
cargo build
export ADJ_BIN="$PWD/target/debug/adj"
# Capture ONLY the two assignment lines setup.sh prints — installer chatter on
# stdout must not reach eval.
eval "$(ci/runtime-compat/fixtures/mise-shim-python/setup.sh \
  | grep -E '^(SHELL_PATH_ADD|LAUNCHD_EXTRA_PATH)=' | sed 's/^/export /')"
PATH="$SHELL_PATH_ADD:$PATH" ci/runtime-compat/run-cell.sh ci/runtime-compat/fixtures/mise-shim-python shell
```
Expected: a line ending `observed=3.11.9 expect=resolved status=pass` and exit 0.

- [ ] **Step 3: Run the cell in `launchd` context — verify FALLBACK**

Run:
```bash
LAUNCHD_EXTRA_PATH="" ci/runtime-compat/run-cell.sh ci/runtime-compat/fixtures/mise-shim-python launchd
```
Expected: `observed` is the system Python (e.g. `3.12.x`), NOT `3.11.9`, and the line ends `expect=fallback status=pass`. (If your system Python happens to be 3.11.9, change the pin to a different installed version and re-run — the pin must differ from the system default.)

- [ ] **Step 4: Commit**

```bash
git add ci/runtime-compat/fixtures/mise-shim-python
git commit ci/runtime-compat/fixtures/mise-shim-python \
  -m "Add mise-shim Python tracer fixture for runtime-compat harness"
```

---

## Task 3: rbenv / Ruby fixture (shim)

**Files:**
- Create: `ci/runtime-compat/fixtures/rbenv-ruby/.ruby-version`
- Create: `ci/runtime-compat/fixtures/rbenv-ruby/adjacent.toml`
- Create: `ci/runtime-compat/fixtures/rbenv-ruby/cell.env`
- Create: `ci/runtime-compat/fixtures/rbenv-ruby/setup.sh`

- [ ] **Step 1: Write the fixture files**

`.ruby-version`:
```
3.3.6
```

`adjacent.toml`:
```toml
name = "app"
cmd = "ruby ../../servers/server.rb"
boot_timeout = 300
```

`cell.env`:
```bash
MANAGER=rbenv
RUNTIME=ruby
PIN=3.3.6
EXPECT_SHELL=resolved
EXPECT_LAUNCHD=fallback
```

`setup.sh`:
```bash
#!/usr/bin/env bash
# Install rbenv + ruby-build + pinned ruby (compiles; slow, cache ~/.rbenv).
set -euo pipefail
export RBENV_ROOT="$HOME/.rbenv"
if [ ! -d "$RBENV_ROOT" ]; then
  git clone --depth 1 https://github.com/rbenv/rbenv.git "$RBENV_ROOT"
  git clone --depth 1 https://github.com/rbenv/ruby-build.git "$RBENV_ROOT/plugins/ruby-build"
fi
export PATH="$RBENV_ROOT/bin:$RBENV_ROOT/shims:$PATH"
eval "$(rbenv init - bash)"
rbenv install -s 3.3.6
rbenv rehash
echo "SHELL_PATH_ADD=$RBENV_ROOT/shims:$RBENV_ROOT/bin"
echo "LAUNCHD_EXTRA_PATH="
```
```bash
chmod +x ci/runtime-compat/fixtures/rbenv-ruby/setup.sh
```

- [ ] **Step 2: Syntax-check (full run is CI-verified — ruby compile is slow locally)**

Run:
```bash
bash -n ci/runtime-compat/fixtures/rbenv-ruby/setup.sh && echo "OK"
```
Expected: `OK`. (Behavioral verification happens in CI, Task 11.)

- [ ] **Step 3: Commit**

```bash
git add ci/runtime-compat/fixtures/rbenv-ruby
git commit ci/runtime-compat/fixtures/rbenv-ruby \
  -m "Add rbenv Ruby fixture for runtime-compat harness"
```

---

## Task 4: asdf / Node fixture (shim)

**Files:**
- Create: `ci/runtime-compat/fixtures/asdf-node/.tool-versions`
- Create: `ci/runtime-compat/fixtures/asdf-node/adjacent.toml`
- Create: `ci/runtime-compat/fixtures/asdf-node/cell.env`
- Create: `ci/runtime-compat/fixtures/asdf-node/setup.sh`

- [ ] **Step 1: Write the fixture files**

`.tool-versions`:
```
nodejs 18.20.5
```

`adjacent.toml`:
```toml
name = "app"
cmd = "node ../../servers/server.js"
boot_timeout = 180
```

`cell.env`:
```bash
MANAGER=asdf
RUNTIME=node
PIN=18.20.5
EXPECT_SHELL=resolved
EXPECT_LAUNCHD=fallback
```

`setup.sh`:
```bash
#!/usr/bin/env bash
# Install asdf (classic) + nodejs plugin + pinned node (prebuilt download).
set -euo pipefail
export ASDF_DIR="$HOME/.asdf"
if [ ! -d "$ASDF_DIR" ]; then
  git clone --depth 1 --branch v0.14.1 https://github.com/asdf-vm/asdf.git "$ASDF_DIR"
fi
# shellcheck source=/dev/null
. "$ASDF_DIR/asdf.sh"
asdf plugin add nodejs 2>/dev/null || true
asdf install nodejs 18.20.5
asdf reshim nodejs
echo "SHELL_PATH_ADD=$ASDF_DIR/shims:$ASDF_DIR/bin"
echo "LAUNCHD_EXTRA_PATH="
```
```bash
chmod +x ci/runtime-compat/fixtures/asdf-node/setup.sh
```

- [ ] **Step 2: Syntax-check**

Run:
```bash
bash -n ci/runtime-compat/fixtures/asdf-node/setup.sh && echo "OK"
```
Expected: `OK`.

- [ ] **Step 3: Commit**

```bash
git add ci/runtime-compat/fixtures/asdf-node
git commit ci/runtime-compat/fixtures/asdf-node \
  -m "Add asdf Node fixture for runtime-compat harness"
```

---

## Task 5: mise activate / Ruby — the Berkopec failure case

Mirrors Berkopec's `mise activate` setup launched the natural way (`cmd = "ruby ..."`). The `mise activate` PWD hook never fires under `sh -c`, so the pinned Ruby is not on PATH. Expected `fallback` in **both** contexts — this is the finding the whole effort exists to surface.

**Files:**
- Create: `ci/runtime-compat/fixtures/mise-activate-ruby/.mise.toml`
- Create: `ci/runtime-compat/fixtures/mise-activate-ruby/adjacent.toml`
- Create: `ci/runtime-compat/fixtures/mise-activate-ruby/cell.env`
- Create: `ci/runtime-compat/fixtures/mise-activate-ruby/setup.sh`

- [ ] **Step 1: Write the fixture files**

`.mise.toml`:
```toml
[tools]
ruby = "3.3.6"
```

`adjacent.toml`:
```toml
name = "app"
cmd = "ruby ../../servers/server.rb"
boot_timeout = 300
```

`cell.env`:
```bash
MANAGER=mise-activate
RUNTIME=ruby
PIN=3.3.6
EXPECT_SHELL=fallback
EXPECT_LAUNCHD=fallback
```

`setup.sh` — installs the runtime and activates mise *in a neutral dir*, deliberately NOT exporting shims. The shell context inherits an activated mise whose PWD hook won't re-fire for the app dir, so Ruby falls back:
```bash
#!/usr/bin/env bash
# Berkopec profile: mise wired via the activation hook, no shims exported.
set -euo pipefail
if ! command -v mise >/dev/null 2>&1; then
  curl -fsSL https://mise.run | sh
fi
export PATH="$HOME/.local/bin:$PATH"
mise install ruby@3.3.6
# Activation hook only: emulate `mise activate` from a NON-fixture dir.
# We intentionally do NOT add mise shims to SHELL_PATH_ADD.
eval "$(mise activate bash)"
echo "SHELL_PATH_ADD=$HOME/.local/bin"
echo "LAUNCHD_EXTRA_PATH="
```
```bash
chmod +x ci/runtime-compat/fixtures/mise-activate-ruby/setup.sh
```

- [ ] **Step 2: Syntax-check**

Run:
```bash
bash -n ci/runtime-compat/fixtures/mise-activate-ruby/setup.sh && echo "OK"
```
Expected: `OK`.

- [ ] **Step 3: Commit**

```bash
git add ci/runtime-compat/fixtures/mise-activate-ruby
git commit ci/runtime-compat/fixtures/mise-activate-ruby \
  -m "Add mise-activate Ruby fixture (Berkopec failure case)"
```

---

## Task 6: mise exec / Node — exec-wrapped workaround

`cmd = "mise exec -- node ..."` self-resolves from `.mise.toml` regardless of shell hooks, as long as the `mise` binary is on PATH. The launchd context puts only `mise`'s dir on the bare PATH (`LAUNCHD_EXTRA_PATH`) — this is the ❓ cell: does the workaround survive launchd? First run is `record`.

**Files:**
- Create: `ci/runtime-compat/fixtures/mise-exec-node/.mise.toml`
- Create: `ci/runtime-compat/fixtures/mise-exec-node/adjacent.toml`
- Create: `ci/runtime-compat/fixtures/mise-exec-node/cell.env`
- Create: `ci/runtime-compat/fixtures/mise-exec-node/setup.sh`

- [ ] **Step 1: Write the fixture files**

`.mise.toml`:
```toml
[tools]
node = "18.20.5"
```

`adjacent.toml`:
```toml
name = "app"
cmd = "mise exec -- node ../../servers/server.js"
boot_timeout = 180
```

`cell.env`:
```bash
MANAGER=mise-exec
RUNTIME=node
PIN=18.20.5
EXPECT_SHELL=resolved
EXPECT_LAUNCHD=record
```

`setup.sh`:
```bash
#!/usr/bin/env bash
# mise exec wrapper: needs the mise binary reachable, not its shims.
set -euo pipefail
if ! command -v mise >/dev/null 2>&1; then
  curl -fsSL https://mise.run | sh
fi
export PATH="$HOME/.local/bin:$PATH"
mise install node@18.20.5
echo "SHELL_PATH_ADD=$HOME/.local/bin"
# launchd: expose ONLY the mise binary dir on the bare PATH.
echo "LAUNCHD_EXTRA_PATH=$HOME/.local/bin"
```
```bash
chmod +x ci/runtime-compat/fixtures/mise-exec-node/setup.sh
```

- [ ] **Step 2: Syntax-check**

Run:
```bash
bash -n ci/runtime-compat/fixtures/mise-exec-node/setup.sh && echo "OK"
```
Expected: `OK`.

- [ ] **Step 3: Commit**

```bash
git add ci/runtime-compat/fixtures/mise-exec-node
git commit ci/runtime-compat/fixtures/mise-exec-node \
  -m "Add mise-exec Node fixture for runtime-compat harness"
```

---

## Task 7: mise run / Ruby — the Berkopec remedy

Same `.mise.toml` as Task 5, but the cmd is `mise run dev` (his task-centric style). The task is defined in `.mise.toml` and self-resolves. Validates the remedy we'd actually hand him; launchd is `record`.

**Files:**
- Create: `ci/runtime-compat/fixtures/mise-run-ruby/.mise.toml`
- Create: `ci/runtime-compat/fixtures/mise-run-ruby/adjacent.toml`
- Create: `ci/runtime-compat/fixtures/mise-run-ruby/cell.env`
- Create: `ci/runtime-compat/fixtures/mise-run-ruby/setup.sh`

- [ ] **Step 1: Write the fixture files**

`.mise.toml`:
```toml
[tools]
ruby = "3.3.6"

[tasks.dev]
run = "ruby ../../servers/server.rb"
```

`adjacent.toml`:
```toml
name = "app"
cmd = "mise run dev"
boot_timeout = 300
```

`cell.env`:
```bash
MANAGER=mise-run
RUNTIME=ruby
PIN=3.3.6
EXPECT_SHELL=resolved
EXPECT_LAUNCHD=record
```

`setup.sh`:
```bash
#!/usr/bin/env bash
# Berkopec remedy: drive the app through a mise task.
set -euo pipefail
if ! command -v mise >/dev/null 2>&1; then
  curl -fsSL https://mise.run | sh
fi
export PATH="$HOME/.local/bin:$PATH"
mise install ruby@3.3.6
echo "SHELL_PATH_ADD=$HOME/.local/bin"
echo "LAUNCHD_EXTRA_PATH=$HOME/.local/bin"
```
```bash
chmod +x ci/runtime-compat/fixtures/mise-run-ruby/setup.sh
```

- [ ] **Step 2: Syntax-check**

Run:
```bash
bash -n ci/runtime-compat/fixtures/mise-run-ruby/setup.sh && echo "OK"
```
Expected: `OK`.

- [ ] **Step 3: Commit**

```bash
git add ci/runtime-compat/fixtures/mise-run-ruby
git commit ci/runtime-compat/fixtures/mise-run-ruby \
  -m "Add mise-run Ruby fixture (Berkopec remedy)"
```

---

## Task 8: uv / Python

`cmd = "uv run python ..."` reads `.python-version` and provisions an ephemeral env. Single binary; ❓ under launchd with only `uv` on the bare PATH.

**Files:**
- Create: `ci/runtime-compat/fixtures/uv-python/.python-version`
- Create: `ci/runtime-compat/fixtures/uv-python/adjacent.toml`
- Create: `ci/runtime-compat/fixtures/uv-python/cell.env`
- Create: `ci/runtime-compat/fixtures/uv-python/setup.sh`

- [ ] **Step 1: Write the fixture files**

`.python-version`:
```
3.11.9
```

`adjacent.toml`:
```toml
name = "app"
cmd = "uv run --python 3.11.9 python ../../servers/server.py"
boot_timeout = 180
```

`cell.env`:
```bash
MANAGER=uv
RUNTIME=python
PIN=3.11.9
EXPECT_SHELL=resolved
EXPECT_LAUNCHD=record
```

`setup.sh`:
```bash
#!/usr/bin/env bash
# Install uv + the managed CPython it will run.
set -euo pipefail
if ! command -v uv >/dev/null 2>&1; then
  curl -fsSL https://astral.sh/uv/install.sh | sh
fi
export PATH="$HOME/.local/bin:$PATH"
uv python install 3.11.9
echo "SHELL_PATH_ADD=$HOME/.local/bin"
echo "LAUNCHD_EXTRA_PATH=$HOME/.local/bin"
```
```bash
chmod +x ci/runtime-compat/fixtures/uv-python/setup.sh
```

- [ ] **Step 2: Syntax-check**

Run:
```bash
bash -n ci/runtime-compat/fixtures/uv-python/setup.sh && echo "OK"
```
Expected: `OK`.

- [ ] **Step 3: Commit**

```bash
git add ci/runtime-compat/fixtures/uv-python
git commit ci/runtime-compat/fixtures/uv-python \
  -m "Add uv Python fixture for runtime-compat harness"
```

---

## Task 9: nvm / Node — activation-only, expected fallback

nvm is a shell function with no binary and no per-directory shim. `cmd = "node ..."` resolves whatever `node` is default. Expected `fallback` in both contexts; the default node must differ from the `.nvmrc` pin.

**Files:**
- Create: `ci/runtime-compat/fixtures/nvm-node/.nvmrc`
- Create: `ci/runtime-compat/fixtures/nvm-node/adjacent.toml`
- Create: `ci/runtime-compat/fixtures/nvm-node/cell.env`
- Create: `ci/runtime-compat/fixtures/nvm-node/setup.sh`

- [ ] **Step 1: Write the fixture files**

`.nvmrc`:
```
18.20.5
```

`adjacent.toml`:
```toml
name = "app"
cmd = "node ../../servers/server.js"
boot_timeout = 120
```

`cell.env`:
```bash
MANAGER=nvm
RUNTIME=node
PIN=18.20.5
EXPECT_SHELL=fallback
EXPECT_LAUNCHD=fallback
```

`setup.sh` — installs nvm and a DIFFERENT default node (22.x) so the fallback is observable, plus the pinned 18.20.5 (to prove even the installed-but-not-selected version isn't auto-chosen):
```bash
#!/usr/bin/env bash
# nvm is a shell function: no shim, no per-dir resolution under sh -c.
set -euo pipefail
export NVM_DIR="$HOME/.nvm"
if [ ! -s "$NVM_DIR/nvm.sh" ]; then
  git clone --depth 1 https://github.com/nvm-sh/nvm.git "$NVM_DIR"
fi
# shellcheck source=/dev/null
. "$NVM_DIR/nvm.sh"
nvm install 22 >/dev/null
nvm install 18.20.5 >/dev/null
nvm alias default 22 >/dev/null
# Export the default node's bin dir (NOT a per-dir shim) for the shell context.
echo "SHELL_PATH_ADD=$(dirname "$(nvm which default)")"
echo "LAUNCHD_EXTRA_PATH="
```
```bash
chmod +x ci/runtime-compat/fixtures/nvm-node/setup.sh
```

- [ ] **Step 2: Syntax-check**

Run:
```bash
bash -n ci/runtime-compat/fixtures/nvm-node/setup.sh && echo "OK"
```
Expected: `OK`.

- [ ] **Step 3: Commit**

```bash
git add ci/runtime-compat/fixtures/nvm-node
git commit ci/runtime-compat/fixtures/nvm-node \
  -m "Add nvm Node fixture for runtime-compat harness"
```

---

## Task 10: Local runner README

**Files:**
- Create: `ci/runtime-compat/README.md`

- [ ] **Step 1: Write the README**

`ci/runtime-compat/README.md`:
```markdown
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
```

- [ ] **Step 2: Commit**

```bash
git add ci/runtime-compat/README.md
git commit ci/runtime-compat/README.md \
  -m "Document how to run runtime-compat cells locally"
```

---

## Task 11: CI workflow — ubuntu matrix + aggregate

**Files:**
- Create: `.github/workflows/runtime-compat.yml`

- [ ] **Step 1: Write the workflow**

`.github/workflows/runtime-compat.yml`:
```yaml
name: runtime-compat

on:
  pull_request:
    paths:
      - "ci/runtime-compat/**"
      - ".github/workflows/runtime-compat.yml"
      - "crates/**"
  workflow_dispatch:

permissions:
  contents: read

jobs:
  cell:
    strategy:
      fail-fast: false
      matrix:
        fixture:
          - mise-shim-python
          - rbenv-ruby
          - asdf-node
          - mise-activate-ruby
          - mise-exec-node
          - mise-run-ruby
          - uv-python
          - nvm-node
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6

      - name: Install Rust toolchain
        uses: dtolnay/rust-toolchain@stable
        with:
          toolchain: "1.92.0"

      - uses: Swatinem/rust-cache@v2

      - name: Build adj
        run: cargo build -p adj

      - name: Cache manager installs
        uses: actions/cache@v4
        with:
          path: |
            ~/.rbenv
            ~/.asdf
            ~/.nvm
            ~/.local/share/mise
            ~/.local/share/uv
            ~/.local/bin
          key: rtcompat-${{ matrix.fixture }}-${{ hashFiles(format('ci/runtime-compat/fixtures/{0}/setup.sh', matrix.fixture)) }}

      - name: Run cell (both contexts)
        run: |
          set -euo pipefail
          export ADJ_BIN="$PWD/target/debug/adj"
          cell="ci/runtime-compat/fixtures/${{ matrix.fixture }}"
          # Run setup in-pipeline (not inside $()) so a failed install aborts the
          # job via `set -o pipefail` instead of being masked by command substitution.
          "$cell/setup.sh" | tee setup.out
          eval "$(grep -E '^(SHELL_PATH_ADD|LAUNCHD_EXTRA_PATH)=' setup.out | sed 's/^/export /')"
          mkdir -p results
          PATH="$SHELL_PATH_ADD:$PATH" \
            ci/runtime-compat/run-cell.sh "$cell" shell   | tee results/${{ matrix.fixture }}-shell.txt
          ci/runtime-compat/run-cell.sh "$cell" launchd   | tee results/${{ matrix.fixture }}-launchd.txt

      - name: Upload result
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: result-${{ matrix.fixture }}
          path: results/

  matrix-report:
    needs: cell
    if: always()
    runs-on: ubuntu-latest
    steps:
      - uses: actions/download-artifact@v4
        with:
          path: results
      - name: Print observed matrix
        run: |
          echo "## Runtime-manager compatibility matrix" >> "$GITHUB_STEP_SUMMARY"
          echo '```' >> "$GITHUB_STEP_SUMMARY"
          grep -rh '^RESULT ' results | sort >> "$GITHUB_STEP_SUMMARY"
          echo '```' >> "$GITHUB_STEP_SUMMARY"
          cat "$GITHUB_STEP_SUMMARY"
```

- [ ] **Step 2: Validate the workflow YAML**

Run:
```bash
python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/runtime-compat.yml')); print('YAML OK')"
```
Expected: `YAML OK`.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/runtime-compat.yml
git commit .github/workflows/runtime-compat.yml \
  -m "Add runtime-compat CI workflow (ubuntu matrix + aggregate report)"
```

---

## Task 12: macOS launchd smoke job + run CI + record results

**Files:**
- Modify: `.github/workflows/runtime-compat.yml` (add a `macos-launchd-smoke` job)
- Modify: `docs/superpowers/specs/2026-06-28-runtime-manager-compatibility-design.md` (append observed RESULTS)

- [ ] **Step 1: Add the macOS launchd smoke job**

Append to `.github/workflows/runtime-compat.yml`:
```yaml
  macos-launchd-smoke:
    runs-on: macos-14
    steps:
      - uses: actions/checkout@v6
      - uses: dtolnay/rust-toolchain@stable
        with:
          toolchain: "1.92.0"
      - uses: Swatinem/rust-cache@v2
      - name: Build adj
        run: cargo build -p adj
      - name: Boot the mise-run fixture under a real launchd agent
        run: |
          set -euo pipefail
          export ADJ_BIN="$PWD/target/debug/adj"
          cell="ci/runtime-compat/fixtures/mise-run-ruby"
          # Run setup in-pipeline (not inside $()) so a failed install aborts the
          # job via `set -o pipefail` instead of being masked by command substitution.
          "$cell/setup.sh" | tee setup.out
          eval "$(grep -E '^(SHELL_PATH_ADD|LAUNCHD_EXTRA_PATH)=' setup.out | sed 's/^/export /')"
          home="$(mktemp -d)"
          export ADJACENT_HOME="$home"   # the client (adj add) must target the daemon's socket
          label="ac.adj.smoke"
          plist="$HOME/Library/LaunchAgents/$label.plist"
          mkdir -p "$(dirname "$plist")"
          cat > "$plist" <<PLIST
          <?xml version="1.0" encoding="UTF-8"?>
          <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
          <plist version="1.0"><dict>
            <key>Label</key><string>$label</string>
            <key>ProgramArguments</key><array>
              <string>$ADJ_BIN</string><string>daemon</string></array>
            <key>EnvironmentVariables</key><dict>
              <key>ADJACENT_HOME</key><string>$home</string>
              <key>ADJACENT_PROXY_PORT</key><string>0</string>
              <key>ADJACENT_HTTPS_PORT</key><string>0</string>
              <key>PATH</key><string>/usr/bin:/bin:/usr/sbin:/sbin:$HOME/.local/bin</string>
            </dict>
            <key>RunAtLoad</key><true/>
          </dict></plist>
          PLIST
          launchctl load "$plist"
          for i in $(seq 1 100); do [ -s "$home/proxy.port" ] && break; sleep 0.1; done
          port="$(cat "$home/proxy.port")"
          "$ADJ_BIN" add "$(cd "$cell" && pwd)"
          out="$(curl -fsS --max-time 120 -H 'Host: app.adj.ac' "http://127.0.0.1:$port/")"
          echo "launchd smoke observed: $out"
          launchctl unload "$plist"
          case "$out" in RUBY*) echo "smoke OK" ;; *) echo "smoke FAILED" >&2; exit 1 ;; esac
```

- [ ] **Step 2: Validate YAML again**

Run:
```bash
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/runtime-compat.yml')); print('YAML OK')"
```
Expected: `YAML OK`.

- [ ] **Step 3: Push the branch and open a draft PR so CI runs**

Use the project agent identity (`gh auth switch -u nonreagent`, `gh auth setup-git`, push, then open a **draft** PR with `Resolves #<issue>` if one exists). Request review from `nonrational`. Do **not** merge.

```bash
git push -u origin runtime-manager-compat
gh pr create --draft --title "Runtime-manager compatibility characterization" \
  --body "Empirical matrix of how adj resolves rbenv/asdf/mise/uv/nvm across inherited-shell vs launchd-minimal contexts. See docs/superpowers/specs/2026-06-28-runtime-manager-compatibility-design.md." \
  --reviewer nonrational
gh auth switch -u nonrational
```

- [ ] **Step 4: Read the observed matrix from the CI run**

After the `runtime-compat` workflow finishes, open the `matrix-report` job summary (or `gh run view --log`). Copy the sorted `RESULT ...` lines.

- [ ] **Step 5: Record results and resolve the ❓ cells**

Append a `## Observed results (<date of run>)` section to the spec with the `RESULT` table. For each `record` cell (mise-exec/mise-run/uv under launchd), change its `cell.env` `EXPECT_LAUNCHD` from `record` to the observed truth (`resolved` or `fallback`) so future runs regress against reality. Note any cell whose result diverged from the design's expectation as a fix-vs-document follow-up.

```bash
git commit ci/runtime-compat/fixtures/*/cell.env \
  docs/superpowers/specs/2026-06-28-runtime-manager-compatibility-design.md \
  -m "Record observed runtime-compat matrix and pin the resolved expectations"
```

- [ ] **Step 6: Push the follow-up commit**

```bash
gh auth switch -u nonreagent && gh auth setup-git
git push
gh auth switch -u nonrational
```

---

## Self-review notes

- **Spec coverage:** success signal (non-default pin, asserted) → `assert_expectation` + per-cell `PIN`/`EXPECT_*` (Task 1–9); two contexts → `start_daemon` shell/launchd (Task 1); all 8 cells → Tasks 2–9; Berkopec profile → Tasks 5 & 7; Linux matrix → Task 11; macOS launchd smoke → Task 12; matrix artifact → `matrix-report` (Task 11) + recorded results (Task 12). Deferred managers (pyenv/direnv) correctly absent.
- **❓ cells** start as `record` (always-pass, log-only) and are pinned to observed truth in Task 12 — honest about what we don't yet know.
- **No silent caps:** every cell emits a `RESULT` line in both contexts; the aggregate prints all of them.
