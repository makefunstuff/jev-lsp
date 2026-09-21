#!/usr/bin/env bash
# The OpenCode bridge's one claim, checked both ways: a rule finding reaches a client shaped
# like OpenCode 1.18 through the bridge, and does **not** reach the same client on the native
# `jev-lsp --stdio` path (issue #21, the flake that motivated the bridge).
#
#   bash editors/opencode/verify-bridge.sh [<server-bin>]
#
#   <server-bin>   default `target/release/jev-lsp` under the repository root.
#
# Both runs use `editors/opencode/opencode_probe.py`, which does exactly what OpenCode 1.18
# does and nothing else: `refreshSupport: false`, one pull on open whose empty answer it keeps,
# no `didSave`, and everything else read from `textDocument/publishDiagnostics`. The two runs
# share a fixture, a stub model and a port, so a green first run cannot come from the fixture
# having produced no finding at all: the second run asserts the finding is *there*.
#
# The stub model's decision call is stalled (`BRIDGE_STUB_DELAY_MS`, 1.2 s by default) so the
# native client's single pull always lands before the ambient pass caches its finding. Without
# the stall the native run would be a race that a fast machine sometimes wins, which is exactly
# the "often empty" the issue reports; with it, the reproduction is deterministic.
#
# No key, no network: the decide tier is `verify/stub_model.py`, imported here exactly as
# `verify/omp_lsp.sh` does it. A missing binary, a busy port or an unanswered stub is a SKIP
# with the reason — never a false ok. Bounded waits only.
set -u

HERE="$(cd "$(dirname "$(realpath "$0")")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
SERVER="${1:-$REPO/target/release/jev-lsp}"
BRIDGE="$HERE/jev-lsp-opencode-bridge.py"
PROBE="$HERE/opencode_probe.py"
STUB="$REPO/verify/stub_model.py"
PORT="${BRIDGE_STUB_PORT:-8098}"
DELAY_MS="${BRIDGE_STUB_DELAY_MS:-1200}"
ORIGIN="http://127.0.0.1:$PORT"
STUB_URL="$ORIGIN/v1"
OUT="${OPENCODE_BRIDGE_OUT:-/tmp/jev-opencode-bridge.log}"
NATIVE_TIMEOUT="${BRIDGE_NATIVE_TIMEOUT:-10}"
BRIDGE_TIMEOUT="${BRIDGE_TIMEOUT:-60}"

: > "$OUT"

STUB_PID=""
cleanup() {
  [ -n "$STUB_PID" ] && kill "$STUB_PID" 2>/dev/null
  wait "$STUB_PID" 2>/dev/null
  return 0
}
trap cleanup EXIT

# --- environment ---------------------------------------------------------------------------------

if [ ! -x "$SERVER" ]; then
  echo "SKIP  no server binary at $SERVER (build it, or pass one as an argument)" | tee -a "$OUT"
  echo "[opencode-bridge] 0 failure(s), 1 skip(s)" | tee -a "$OUT"
  exit 0
fi
if ! command -v python3 >/dev/null 2>&1; then
  echo "SKIP  python3 is not on PATH" | tee -a "$OUT"
  echo "[opencode-bridge] 0 failure(s), 1 skip(s)" | tee -a "$OUT"
  exit 0
fi

# The port is this run's. If something already answers there it belongs to a sibling run, and
# killing it would turn that run red — so refuse, naming the holder, exactly as run-suite.sh does.
if curl -fsS -o /dev/null --max-time 1 "$ORIGIN/health" 2>/dev/null; then
  echo "SKIP  something already answers $ORIGIN/health; set BRIDGE_STUB_PORT=<other> for this run" \
    | tee -a "$OUT"
  echo "[opencode-bridge] 0 failure(s), 1 skip(s)" | tee -a "$OUT"
  exit 0
fi

STUB_PORT="$PORT" STUB_DELAY_MS="$DELAY_MS" python3 "$STUB" >>"$OUT" 2>&1 &
STUB_PID=$!
for _ in $(seq 1 40); do
  curl -fsS -o /dev/null --max-time 1 "$ORIGIN/health" 2>/dev/null && break
  sleep 0.25
done
if ! curl -fsS -o /dev/null --max-time 1 "$ORIGIN/health" 2>/dev/null; then
  echo "SKIP  the stub never answered $ORIGIN/health" | tee -a "$OUT"
  echo "[opencode-bridge] 0 failure(s), 1 skip(s)" | tee -a "$OUT"
  exit 0
fi
printf 'ok    stub model on %s (decision delay %s ms)\n' "$STUB_URL" "$DELAY_MS" | tee -a "$OUT"

# --- fixture -------------------------------------------------------------------------------------

RULES='{"schema":"jev.rules/1","rules":[{"id":"no-unwrap-in-handlers","title":"Unwrap in a request handler","text":"A handler must not unwrap; return the error instead.","severity":"warning","applies_to":["**/*.rs"],"inspection":{"kind":"regex","pattern":"\\.unwrap\\(\\)","max_matches":0},"judgement":{"question":"Is this unwrap reachable from a request handler?","criteria":{"true":"the call sits on a path a request can reach","false":"the call is in a test, a startup path, or behind an invariant"},"min_probability":0.75},"verb_hint":"fix"}]}'
TITLE='Unwrap in a request handler'

FIXTURE="$(mktemp -d /tmp/jev-opencode-bridge.XXXXXX)"
trap 'rm -rf "$FIXTURE"; cleanup' EXIT
mkdir -p "$FIXTURE/.git" "$FIXTURE/.jev/rules"
cat > "$FIXTURE/handler.rs" <<'RS'
use std::fs;

pub fn handle(path: &str) -> String {
    let body = fs::read_to_string(path).unwrap();
    body
}
RS
printf '%s\n' "$RULES" > "$FIXTURE/.jev/rules/example.json"

# --- the real client, when it is installed -------------------------------------------------------

# The two probe stages below run a client written to OpenCode's behaviour, which is what makes the
# mechanism checkable. This stage runs OpenCode itself, with `opencode debug lsp diagnostics
# <file>`: the command issue #21 was filed from, booting the configured server and printing what
# the editor holds for the file. `opencode` missing from `PATH` is a SKIP that names the reason,
# never a failure, and the row still has the probe stages.
OC="$(command -v opencode 2>/dev/null || true)"

oc_config() { # $1 = the `command` array, as JSON
  printf '{\n  "$schema": "https://opencode.ai/config.json",\n  "lsp": {\n    "jev": {\n      "command": %s,\n      "extensions": [".rs"],\n      "env": {\n        "JEV_LSP_BIN": "%s",\n        "JEV_DECIDE_BASE_URL": "%s",\n        "JEV_DECIDE_MODEL": "stub-model",\n        "JEV_DECIDE_WIRE": "system_one",\n        "JEV_DECIDE_TIMEOUT_MS": "15000"\n      }\n    },\n    "rust": { "disabled": true }\n  }\n}\n' \
    "$1" "$SERVER" "$STUB_URL" > "$FIXTURE/opencode.json"
}

# The stage's own check, on the JSON `debug lsp diagnostics` prints: the entry for `handler.rs`
# has to be empty (native), or to carry a severity-1 diagnostic whose message names the rule and
# carries the bridge's prefix (bridge).
check_real() { # $1 = mode, $2 = json file
  python3 - "$1" "$2" "$TITLE" <<'PY'
import json, sys

mode, path, title = sys.argv[1], sys.argv[2], sys.argv[3]
try:
    data = json.load(open(path, encoding="utf-8"))
except Exception as error:
    print("FAIL  `opencode debug lsp diagnostics` printed no readable JSON: %s" % error)
    sys.exit(1)
entries = [v for k, v in data.items() if k.endswith("handler.rs")]
if not entries:
    print("FAIL  the output names no entry for handler.rs: %s" % list(data))
    sys.exit(1)
diagnostics = entries[0]
if mode == "native":
    if diagnostics:
        print("FAIL  the native path surfaced %d diagnostic(s); the issue expects none" % len(diagnostics))
        sys.exit(1)
    print("ok    opencode debug lsp diagnostics: native jev-lsp --stdio reports nothing for the rule")
    sys.exit(0)
shown = [d for d in diagnostics if title in (d.get("message") or "")]
if not shown:
    print("FAIL  no diagnostic names the rule %r; got %s" % (title, diagnostics))
    sys.exit(1)
if not all(d.get("severity") == 1 for d in shown):
    print("FAIL  the bridge is expected to hand OpenCode severity 1; got %s"
          % [d.get("severity") for d in shown])
    sys.exit(1)
if not any((d.get("message") or "").startswith("[jev warning]") for d in shown):
    print("FAIL  the message lost the [jev warning] prefix: %s" % [d.get("message") for d in shown])
    sys.exit(1)
print("ok    opencode debug lsp diagnostics: the bridge reports the rule at severity 1")
PY
}

run_real() { # $1 = mode (native|bridge), $2 = the `command` array as JSON, $3 = label
  local json="/tmp/jev-opencode-real-$1.json"
  oc_config "$2"
  printf '\n----- opencode debug lsp diagnostics, %s config (expect: %s)\n' "$3" \
    "$([ "$1" = native ] && echo 'nothing' || echo 'the finding')" | tee -a "$OUT"
  ( cd "$FIXTURE" && timeout 120 "$OC" debug lsp diagnostics handler.rs ) >"$json" 2>>"$OUT"
  local status=$?
  cat "$json" >>"$OUT" 2>/dev/null || true
  if [ "$status" -ne 0 ]; then
    printf 'FAIL  `opencode debug lsp diagnostics` exited %s\n' "$status" | tee -a "$OUT"
    return
  fi
  check_real "$1" "$json" | tee -a "$OUT"
}

# --- the two probe runs --------------------------------------------------------------------------

# The decide tier is this harness's stub, named explicitly rather than inherited. `run-suite.sh`
# exports `JEV_DECIDE_*` for its own stub on its own port, and the environment wins over the
# `jev` section a client sends (PROTOCOL §10), so an inherited endpoint would silently replace
# the stalled one below and turn the native run into a race.
PROBE_ENV=(env JEV_DECIDE_BASE_URL="$STUB_URL" JEV_DECIDE_MODEL=stub-model JEV_DECIDE_WIRE=system_one
  JEV_DECIDE_TIMEOUT_MS=15000)

if [ -z "$OC" ]; then
  printf '\nSKIP  opencode is not on PATH; the two probe stages below are the whole check here\n' | tee -a "$OUT"
else
  run_real native "[\"$SERVER\", \"--stdio\"]" "native"
  run_real bridge "[\"python3\", \"$BRIDGE\"]" "bridge"
fi

# 1. Native: the reproduction. Nothing may reach the client, and the record has to say *why*
#    (one empty pull, one acknowledged refresh, zero pushes) rather than merely "nothing".
printf '\n----- native jev-lsp --stdio, no bridge (expect: nothing reaches the client)\n' | tee -a "$OUT"
"${PROBE_ENV[@]}" python3 "$PROBE" --native "$SERVER" --bin "$SERVER" --root "$FIXTURE" --file handler.rs \
  --stub-url "$STUB_URL" --expect empty --timeout "$NATIVE_TIMEOUT" \
  --json-out /tmp/jev-opencode-native.json >/tmp/jev-opencode-native.out 2>>"$OUT"
native_status=$?
cat /tmp/jev-opencode-native.json >>"$OUT" 2>/dev/null || true
if [ "$native_status" -eq 0 ]; then
  printf 'ok    issue #21 reproduced: pull on open is empty, refresh is a no-op, nothing is pushed\n' | tee -a "$OUT"
else
  printf 'FAIL  the native path was expected to leave the client empty; it did not\n' | tee -a "$OUT"
fi

# 2. Bridge: the fix. The same client, the same fixture, the same server, behind the bridge.
printf '\n----- bridge (expect: the finding reaches the client)\n' | tee -a "$OUT"
"${PROBE_ENV[@]}" python3 "$PROBE" --bridge "$BRIDGE" --bin "$SERVER" --root "$FIXTURE" --file handler.rs \
  --stub-url "$STUB_URL" --expect surfaced --timeout "$BRIDGE_TIMEOUT" \
  --json-out /tmp/jev-opencode-bridge.json >/tmp/jev-opencode-bridge.out 2>>"$OUT"
bridge_status=$?
cat /tmp/jev-opencode-bridge.json >>"$OUT" 2>/dev/null || true
if [ "$bridge_status" -eq 0 ]; then
  printf 'ok    the finding reached the OpenCode-shaped client through the bridge\n' | tee -a "$OUT"
else
  printf 'FAIL  the bridge did not surface a finding to an OpenCode-shaped client\n' | tee -a "$OUT"
fi

# The finding itself, checked from outside both clients: the message carries the rule title the
# fixture wrote, and the severity is the one OpenCode's agent transcript shows.
if grep -q "$TITLE" /tmp/jev-opencode-bridge.json; then
  printf 'ok    the pushed diagnostic names the rule: %s\n' "$TITLE" | tee -a "$OUT"
else
  printf 'FAIL  the pushed diagnostic does not name the rule %s\n' "$TITLE" | tee -a "$OUT"
fi

# --- verdict -------------------------------------------------------------------------------------

rm -rf "$FIXTURE"
FAILS=$(grep -cE '^FAIL' "$OUT" || true)
SKIPS=$(grep -cE '^SKIP' "$OUT" || true)
printf '\n[opencode-bridge] %s failure(s), %s skip(s)\n' "$FAILS" "$SKIPS" | tee -a "$OUT"
[ "$FAILS" -eq 0 ] || exit 1
exit 0
