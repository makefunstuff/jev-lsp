#!/usr/bin/env bash
# The verification table, in one run. Usage:
#
#   bash verify/run-suite.sh <out-file> [<server-bin-name>] [<cli-bin-name>]
#
#   <out-file>          every step's stdout/stderr, plus the summary block
#   <server-bin-name>   default `jev-lsp`, under `target/release/`
#   <cli-bin-name>      default `jev`, only used in the header line
#
# Run it from anywhere: the repository root is derived from this script's own location, and every
# step runs from there. Never uses `set -e`: each command runs and its exit code is recorded, so a
# red row is visible instead of aborting the run.
#
# Environment:
#   NVIM_ONLY=1       skip cargo, the probes and every Python/OMP harness; run only the stub
#                     lifecycle and the Lua harnesses. Use it when the Rust tree or the harnesses
#                     are being edited concurrently — mid-flight, a sibling's half-finished edit
#                     is a phantom failure here.
#   REFUSE_IF_BUSY=1  refuse (exit 1, no killing) when 8099 already answers /health, because a
#                     stub that is already up may belong to another suite that is mid-run.
#   NVIM_BINS="…"     space-separated Neovim binaries for the Lua harnesses.
#                     Default: the 0.12.5 build plus the installed `nvim`.
#   JEV_API_KEY_ENV   name of the variable holding the chat tiers' key (default
#                     `OPENROUTER_API_KEY`); the *name* is handed to the harness, never the
#                     value. `quality_eval` needs it — see `JEV_QUALITY_*` below.
#   JEV_QUALITY_BASE_URL / JEV_QUALITY_MODEL
#                     the real-endpoint row's endpoint and model. Defaults
#                     `https://openrouter.ai/api/v1` and `google/gemini-2.5-flash-lite` (the
#                     pair `docs/VERIFICATION.md` §7 records). If the named key variable is
#                     empty or the endpoint does not answer, the row is `?` with the reason —
#                     never a pass, never a failure.

set -u

HERE="$(cd "$(dirname "$(realpath "$0")")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
OUT="${1:?usage: bash verify/run-suite.sh <out-file> [<server-bin-name>] [<cli-bin-name>]}"
BIN_NAME="${2:-jev-lsp}"
CLI_NAME="${3:-jev}"

cd "$REPO" || exit 2
: > "$OUT"

STUB_PORT=8099
STUB_ROOT="http://127.0.0.1:$STUB_PORT"
STUB_URL="$STUB_ROOT/v1"   # the model endpoint the server is pointed at
                           # ($STUB_ROOT/health is the liveness probe)

# --- Stub lifecycle, before anything runs -----------------------------------------------------
#
# A stale stub bound to 8099 answers /health and then serves whatever state it was left in,
# which is the failure mode docs/VERIFICATION.md warns about by name: the harnesses see no
# findings, and a dead endpoint reads exactly like a product defect. So: kill any leftover,
# wait until the port is actually free, start exactly one stub that lives for the whole run, and
# only then let a harness near it. Everything here happens before the first harness.

stub_pid=""
cleanup() {
  if [ -n "$stub_pid" ]; then
    kill "$stub_pid" 2>/dev/null
    wait "$stub_pid" 2>/dev/null
  fi
}
trap cleanup EXIT

# `REFUSE_IF_BUSY=1`: a stub already answering /health may belong to another suite that is
# mid-run, and killing it would turn *that* run red. Nothing here can tell a sibling's live stub
# from a stale one, so refuse rather than kill or adopt. Left unset, the default is unchanged:
# kill the leftover and start a clean one.
if [ "${REFUSE_IF_BUSY:-0}" = "1" ]; then
  if python3 - "$STUB_ROOT" <<'PY'
import sys, urllib.request
try:
    urllib.request.urlopen(sys.argv[1] + "/health", timeout=0.5)
except Exception:
    sys.exit(1)
PY
  then
    {
      echo "### stub port"
      echo "FATAL: $STUB_ROOT/health already answers — refusing to run: this stub may belong to"
      echo "       another suite that is mid-run, and killing it would turn that run red."
      echo "EXIT=1"
    } >> "$OUT"
    echo "FATAL: port $STUB_PORT is already served; refusing (REFUSE_IF_BUSY=1, see $OUT)" >&2
    exit 1
  fi
fi

# Nothing may still be accepting on the port. `pkill -f` matches the script path in argv, so it
# finds a stub however it was launched (relative or absolute path).
pkill -f "verify/stub_model.py" 2>/dev/null
freed=0
for _ in $(seq 1 50); do
  if ! python3 - "$STUB_PORT" <<'PY'
import socket, sys
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.settimeout(0.3)
try:
    s.connect(("127.0.0.1", int(sys.argv[1])))
    sys.exit(0)   # busy: something still accepts on the port
except Exception:
    sys.exit(1)   # free
finally:
    s.close()
PY
  then
    freed=1
    break
  fi
  sleep 0.2
done
if [ "$freed" -ne 1 ]; then
  {
    echo "### stub port"
    echo "FATAL: 127.0.0.1:$STUB_PORT still accepts connections after killing verify/stub_model.py."
    echo "       Refusing to run: a stale stub would serve the harnesses and hide the real endpoint."
  } >> "$OUT"
  echo "FATAL: port $STUB_PORT is not free; aborting (see $OUT)" >&2
  exit 1
fi

python3 verify/stub_model.py >> "$OUT" 2>&1 &
stub_pid=$!
echo "=== stub: pid $stub_pid on $STUB_URL (repo: $REPO  bin: $BIN_NAME  cli: $CLI_NAME  date: $(date -u +%FT%TZ)) ===" >> "$OUT"

# /health, and the process still being alive, both have to hold. Checking the pid matters: if
# another process won the race for the port, our stub exits immediately and /health would still
# be answered — by the wrong stub — while this script carried on.
health_ok=0
for _ in $(seq 1 50); do
  if ! kill -0 "$stub_pid" 2>/dev/null; then
    break
  fi
  if python3 - "$STUB_ROOT" <<'PY'
import sys, urllib.request
try:
    urllib.request.urlopen(sys.argv[1] + "/health", timeout=0.5)
except Exception:
    sys.exit(1)
PY
  then
    health_ok=1
    break
  fi
  sleep 0.2
done
if [ "$health_ok" -ne 1 ]; then
  if ! kill -0 "$stub_pid" 2>/dev/null; then
    reason="the stub process exited before answering (port already taken?)"
  else
    reason="no /health response within 10 s"
  fi
  {
    echo "### stub health"
    echo "FATAL: verify/stub_model.py never served $STUB_ROOT/health — $reason"
    echo "EXIT=1"
    echo
  } >> "$OUT"
  echo "FATAL: stub never answered /health on $STUB_ROOT ($reason); aborting" >&2
  exit 1
fi
echo "### stub health" >> "$OUT"
echo "ok stub $STUB_ROOT/health answered, pid $stub_pid alive" >> "$OUT"
echo "EXIT=0" >> "$OUT"
echo >> "$OUT"

run() {
  local label="$1"; shift
  {
    echo "### $label"
    echo "\$ $*"
  } >> "$OUT"
  "$@" >> "$OUT" 2>&1
  echo "EXIT=$?" >> "$OUT"
  echo >> "$OUT"
}

if [ "${NVIM_ONLY:-0}" != "1" ]; then
  run "cargo test" cargo test
  run "cargo build --release" cargo build --release
  run "probes" bash verify/probes/run.sh

  run "latency" python3 verify/latency.py
  run "queue" python3 verify/queue_test.py
  run "config_race" python3 verify/config_race_test.py
  run "supersede" python3 verify/supersede_probe.py
  run "smoke" python3 verify/smoke.py
  run "outcome" python3 verify/outcome_test.py
  run "plan" python3 verify/plan_test.py
  run "cli_parity" python3 verify/cli_parity.py
  run "lsp_client" python3 verify/lsp_client.py --server "$REPO/target/release/$BIN_NAME" --workspace /tmp/jev-ws --stub-model-url "$STUB_URL"
  # The stdio framing of the test client itself: the regression that made the server look guilty
  # for an intermittent `codeAction` stall. No server, no product code.
  run "lsp_framing_test" python3 verify/lsp_framing_test.py
  # The rules pass, from a second Python client (`smoke.Stub` on its own port, so no collision).
  run "rules_test" python3 verify/rules_test.py --bin "$REPO/target/release/$BIN_NAME"
  # The rules gate, over the shipped CLI and the stub: a seeded violation must exit 1, a file no
  # rule claims 0, and a gate that cannot run 2. This is what a session runs before it commits.
  # The gate needs a decide endpoint, and the runner's exports below sit outside this block by
  # design (they feed the Lua harnesses), so the row names the same stub itself rather than
  # inheriting nothing and skipping.
  run "rules_gate" env JEV_DECIDE_BASE_URL="$STUB_URL" JEV_DECIDE_WIRE=system_one JEV_DECIDE_MODEL=stub-model python3 verify/rules_gate_test.py
fi

# Both spellings: the pre-rename tree reads META_*, the renamed one JEV_*. Each ignores the
# other's, so one script serves both the baseline run and the post-rename run.
export META_LSP_BIN="$PWD/target/release/$BIN_NAME"
export JEV_LSP_BIN="$META_LSP_BIN"
export META_BASE_URL="$STUB_URL"
export JEV_BASE_URL="$META_BASE_URL"
export META_MODEL=stub-model
export JEV_MODEL="$META_MODEL"
# The ambient pass is the rules pass, whose questions go to the decide tier. Point it at the
# stub too, or every save reaches for the default cloud endpoint.
export META_DECIDE_BASE_URL="$STUB_URL"
export JEV_DECIDE_BASE_URL="$META_DECIDE_BASE_URL"
export META_DECIDE_MODEL=stub-model
export JEV_DECIDE_MODEL="$META_DECIDE_MODEL"

# The Lua harnesses run under every Neovim named. The plugin's code-lens path and the LSP
# runtime differ between 0.12.1 and 0.12.5, so "green on one" is not the claim being tested.
NVIM_BINS="${NVIM_BINS:-/tmp/nvim125/nvim-macos-arm64/bin/nvim nvim}"
for nvim_bin in $NVIM_BINS; do
  nvim_tag="$("$nvim_bin" --version 2>/dev/null | sed -n '1s/^NVIM /nvim-/p')"
  [ -n "$nvim_tag" ] || nvim_tag="$nvim_bin"
  run "nvim_live[$nvim_tag]" "$nvim_bin" --headless -u NONE -l verify/nvim_live.lua
  run "dismiss[$nvim_tag]" "$nvim_bin" --headless -u NONE -l verify/dismiss_test.lua
  run "nvim_ui_test[$nvim_tag]" "$nvim_bin" --headless -u NONE -l verify/nvim_ui_test.lua
  run "rules_live[$nvim_tag]" "$nvim_bin" --headless -u NONE -l verify/rules_live.lua
  # Where a result goes: the report takes the buffer in the window the user is already in, and
  # the window count does not change *across* the command rather than merely before and after it
  # (a split that closed itself would pass that weaker check). No knob of its own: it needs
  # JEV_LSP_BIN, and for its one streamed check a chat endpoint, which is the stub this runner
  # has already started and JEV_BASE_URL already points at. A model endpoint that does not answer
  # is a SKIP for that check and never a pass.
  run "result_surface[$nvim_tag]" "$nvim_bin" --headless -u NONE -l verify/result_surface.lua
done

if [ "${NVIM_ONLY:-0}" != "1" ]; then
  # A third client, written by someone else: proves the standard surfaces are enough. It reuses
  # the stub this runner already holds and SKIPs (never FAILs) when `omp` or its model is
  # unavailable.
  run "omp_lsp" bash verify/omp_lsp.sh "$BIN_NAME"

  # The real-endpoint row. `JEV_API_KEY_ENV` names the variable that holds the key — the chat
  # tiers read the *name*, never the value, and the value never reaches this script's output.
  # A missing key or an endpoint that does not answer is `?` with the reason: never a pass, and
  # never a failure. When it does run, the row's exit code is the harness's own, so a red
  # harness is red here.
  QUALITY_KEY_VAR="${JEV_API_KEY_ENV:-OPENROUTER_API_KEY}"
  QUALITY_BASE="${JEV_QUALITY_BASE_URL:-https://openrouter.ai/api/v1}"
  QUALITY_MODEL="${JEV_QUALITY_MODEL:-google/gemini-2.5-flash-lite}"
  QUALITY_WHY=""
  if [ -z "${!QUALITY_KEY_VAR:-}" ]; then
    QUALITY_WHY="JEV_API_KEY_ENV names $QUALITY_KEY_VAR, which is unset or empty"
  elif ! python3 - "$QUALITY_BASE" <<'PY'
import socket, sys, urllib.parse
url = urllib.parse.urlparse(sys.argv[1])
if not url.hostname:
    sys.exit(1)
port = url.port or (443 if url.scheme == "https" else 80)
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.settimeout(3)
try:
    s.connect((url.hostname, port))
except Exception:
    sys.exit(1)
finally:
    s.close()
PY
  then
    QUALITY_WHY="$QUALITY_BASE does not answer (no TCP to its host)"
  fi

  if [ -n "$QUALITY_WHY" ]; then
    {
      echo "### quality_eval"
      echo "? not run: $QUALITY_WHY"
      echo "  (it measures the review against a real model: export the key variable, or set"
      echo "   JEV_QUALITY_BASE_URL / JEV_QUALITY_MODEL for a different endpoint and model)"
    } >> "$OUT"
  else
    run "quality_eval" python3 verify/quality_eval.py \
      --base-url "$QUALITY_BASE" --model "$QUALITY_MODEL"
  fi
fi

# The summary is the artefact, so it goes into the file *and* to stdout: the file is what a
# machine reads afterwards (`sed -n '/^=== summary ===$/,$p' "$OUT"`), and stdout is what a person
# watching the run sees. Appending only the header — which is what this did — left the file with no
# verdicts in it at all, so the gate that reads it could never fire.
#
# The exit code is the aggregate of the `EXIT=` values `run()` recorded, not a text grep over them:
# a row that failed is a red run however it failed, and the text grep is the second net rather than
# the first. The CI step already says the runner's exit code is its last step, the summary; this is
# what makes that sentence true.
echo "=== summary ===" >> "$OUT"
summary_code=0
python3 - "$OUT" <<'PY' || summary_code=$?
import re, sys

path = sys.argv[1]
txt = open(path).read()
blocks = re.split(r"^### ", txt, flags=re.M)[1:]
rows, failed = [], False
for b in blocks:
    label = b.splitlines()[0]
    exits = re.findall(r"^EXIT=(\d+)", b, flags=re.M)
    code = int(exits[-1]) if exits else None
    verdict = "ok" if code == 0 else ("FAIL" if code is not None else "?")
    # A row with no `EXIT=` is `?`: it could not run, and it says why in its own block (the
    # quality row with no key). Not a pass, and not a failure either.
    if code not in (0, None):
        failed = True
    rows.append(f"{verdict:4} {label}")
summary = "\n".join(rows) + "\n"
with open(path, "a") as fh:
    fh.write(summary)
sys.stdout.write(summary)
sys.exit(1 if failed else 0)
PY

exit "$summary_code"
