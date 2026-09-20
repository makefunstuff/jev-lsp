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
#   TAKE_OVER=1       take the stub port even if something is already serving it: kill the
#                     leftover and start a clean stub. This is for a machine with one session on
#                     it — it is not the default, because that same kill would turn a *sibling's*
#                     mid-run suite red (its harnesses would talk to this run's stub, and the
#                     ones that read counters would be reading another run's).
#                     Default: refuse, with the pid and command line of whatever holds the port.
#                     `REFUSE_IF_BUSY=0` is the older spelling of this opt-in and still means it.
#   STUB_PORT         the port the stub is started on and the model endpoints point at
#                     (default 8099). The answer for two suites on one machine at the same time:
#                     give the second one `STUB_PORT=<other>`, and every harness follows, because
#                     they read the endpoint they were handed (`JEV_BASE_URL`).
#   JEV_WS            the `lsp_client` row's workspace (default `/tmp/jev-ws`). The default is
#                     this run's to remove when it ends; a path named here is the caller's and is
#                     left alone, exactly as the Lua rows treat `JEV_ROOT`.
#   KEEP_WORKSPACE=1  keep even the default workspace, so a failing row can be read afterwards.
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

STUB_PORT="${STUB_PORT:-8099}"
STUB_ROOT="http://127.0.0.1:$STUB_PORT"
STUB_URL="$STUB_ROOT/v1"   # the model endpoint the server is pointed at
                           # ($STUB_ROOT/health is the liveness probe)

# --- The summary ------------------------------------------------------------------------------
#
# Defined before the stub lifecycle, because that is where the script can exit early: a run that
# refuses the port still has to leave a file a machine can read. It writes the verdicts to `$OUT`
# *and* to stdout — the file is the artefact (`sed -n '/^=== summary ===$/,$p' "$OUT"`), stdout is
# what a person watching sees — and returns non-zero if any recorded row failed, so a red row is a
# red exit however it failed and the text grep in CI is the second net rather than the first.

emit_summary() {
  echo "=== summary ===" >> "$OUT"
  python3 - "$OUT" <<'PY'
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
    # quality row without a key, the port this run refused). Not a pass, and not a failure either.
    if code not in (0, None):
        failed = True
    rows.append(f"{verdict:4} {label}")
summary = "\n".join(rows) + "\n"
with open(path, "a") as fh:
    fh.write(summary)
sys.stdout.write(summary)
sys.exit(1 if failed else 0)
PY
}

# --- Stub lifecycle, before anything runs -----------------------------------------------------
#
# A stale stub bound to the port answers /health and then serves whatever state it was left in,
# which is the failure mode docs/VERIFICATION.md warns about by name: the harnesses see no
# findings, and a dead endpoint reads exactly like a product defect. So exactly one stub serves a
# run, and the port is settled before the first harness.
#
# What is *not* settled by looking at the port is whose stub it is. Two states look identical
# from here — a leftover nobody is using, and a sibling session's live stub — and only the second
# one matters: killing it turns that session's run red, twice over (its harnesses would talk to
# this run's fresh stub, and the ones that read counters would be reading another run's). So the
# default is to *refuse*, naming the holder, and taking the port is the deliberate act
# (`TAKE_OVER=1`, for a machine with one session on it).

# The `lsp_client` row's workspace, and the same rule as the Lua rows' fixture roots: a path this
# runner created is removed when the run ends (every exit path, through the `trap` below), and a
# `JEV_WS` the caller named — or `KEEP_WORKSPACE=1` — is left where it is. Leaving it is what makes
# a failing row debuggable; leaving it *every* time is how `/tmp` filled up with repository markers.
WORKSPACE="${JEV_WS:-/tmp/jev-ws}"
workspace_is_ours=0
if [ -z "${JEV_WS:-}" ]; then
  workspace_is_ours=1
fi

stub_pid=""
cleanup() {
  if [ -n "$stub_pid" ]; then
    kill "$stub_pid" 2>/dev/null
    wait "$stub_pid" 2>/dev/null
  fi
  # The exact directory this run used, never a parent and never a pattern.
  if [ "$workspace_is_ours" = "1" ] && [ "${KEEP_WORKSPACE:-0}" != "1" ]; then
    rm -rf "$WORKSPACE"
  fi
}
trap cleanup EXIT

# Something accepting on the port is the fact that matters, and a connection is how it is observed
# — a process name is only how the holder is *named* in the message.
port_busy() {
  python3 - "$STUB_PORT" <<'PY'
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
}

# Who holds it, for the refusal and for the take-over: `lsof` where it exists, the stub's own name
# otherwise. Empty when the holder cannot be named — which is not a reason to run.
port_holder_pid() {
  local pid=""
  if command -v lsof >/dev/null 2>&1; then
    pid="$(lsof -nP -iTCP:"$STUB_PORT" -sTCP:LISTEN -t 2>/dev/null | head -n 1)"
  fi
  if [ -z "$pid" ]; then
    # No `lsof`: the name is all there is, and it cannot tell a stub on this port from a sibling's
    # stub on another one. It is only reached when nothing could be identified by the port.
    pid="$(pgrep -f 'verify/stub_model.py' 2>/dev/null | head -n 1)"
  fi
  printf '%s' "$pid"
}

port_holder() {
  local pid="$1"
  if [ -n "$pid" ]; then
    printf 'pid %s  %s' "$pid" "$(ps -o command= -p "$pid" 2>/dev/null | head -n 1)"
  fi
}

take_over=0
if [ "${TAKE_OVER:-0}" = "1" ] || [ "${REFUSE_IF_BUSY:-1}" = "0" ]; then
  take_over=1
fi

if port_busy; then
  holder_pid="$(port_holder_pid)"
  holder="$(port_holder "$holder_pid")"
  if [ "$take_over" != "1" ]; then
    {
      echo "### stub port"
      echo "FATAL: 127.0.0.1:$STUB_PORT is already served — refusing to run."
      [ -n "$holder" ] && echo "  holder : $holder"
      echo "  why    : a stub answering that port belongs to a suite that is mid-run, and killing it"
      echo "           would turn that run red — its harnesses would talk to this run's stub, and"
      echo "           the ones that read counters would be reading another run's."
      echo "  do     : wait for the other run to finish; or re-run with TAKE_OVER=1 to kill the"
      echo "           leftover and start a fresh stub (the single-session case); or give this run"
      echo "           its own port with STUB_PORT=<port>."
      echo "EXIT=1"
    } >> "$OUT"
    echo "FATAL: 127.0.0.1:$STUB_PORT is already served (${holder:-holder unnamed}); refusing. Use TAKE_OVER=1 to take it, or STUB_PORT=<other port> to run alongside." >&2
    emit_summary
    exit 1
  fi
  echo "=== stub: taking 127.0.0.1:$STUB_PORT from ${holder:-an unnamed holder} (TAKE_OVER=1) ===" >> "$OUT"
fi

# Reached when the port was free, or when the operator asked for it. The leftover is the process on
# *this* port when it can be named: a sibling's stub on another port is not in the way, and killing
# it would be the same mistake as taking this one.
if port_busy; then
  if [ -n "${holder_pid:-}" ]; then
    kill "$holder_pid" 2>/dev/null
  else
    pkill -f "verify/stub_model.py" 2>/dev/null
  fi
fi
freed=0
for _ in $(seq 1 50); do
  if ! port_busy; then
    freed=1
    break
  fi
  sleep 0.2
done
if [ "$freed" -ne 1 ]; then
  {
    echo "### stub port"
    echo "FATAL: 127.0.0.1:$STUB_PORT still accepts connections after killing verify/stub_model.py."
    echo "       Refusing to run: a stub would serve the harnesses and hide the real endpoint."
    echo "EXIT=1"
  } >> "$OUT"
  echo "FATAL: port $STUB_PORT is not free; aborting (see $OUT)" >&2
  emit_summary
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
  # Two harnesses that existed and were never run, each pinning a defect that really shipped: an
  # answer that reaches outside the scope the user selected has to be refused by name, and the
  # first model call of a session has to use the client's configured endpoint rather than the
  # built-in default. `config_race` above is the weaker sibling of the second, and it was the only
  # one of the three in the table.
  run "settings_race" python3 verify/settings_race_test.py --bin "$REPO/target/release/$BIN_NAME"
  run "scope_containment" python3 verify/scope_containment_test.py --bin "$REPO/target/release/$BIN_NAME"
  run "supersede" python3 verify/supersede_probe.py
  run "smoke" python3 verify/smoke.py
  run "outcome" python3 verify/outcome_test.py
  run "plan" python3 verify/plan_test.py
  run "cli_parity" python3 verify/cli_parity.py
  run "lsp_client" python3 verify/lsp_client.py --server "$REPO/target/release/$BIN_NAME" --workspace "$WORKSPACE" --stub-model-url "$STUB_URL"
  # The test client's own defect-injection net: no server, no stub, no network. It injects each
  # defect into its own client and requires the client to catch it, so "the suite is green" is not
  # a net that cannot fail. ~25 s.
  run "lsp_client_selftest" python3 verify/lsp_client.py --selftest
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
  # The client's own local search (`:Jev where`'s grep), on both engines and with neither: no
  # server and no model, so it is the one Lua row that needs no `JEV_LSP_BIN` — the defect it
  # pins is that the fallback answered a different question than `rg` and that "no match" and
  # "the search never ran" were the same empty table.
  run "context_search[$nvim_tag]" "$nvim_bin" --headless -u NONE -l verify/context_search.lua
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

# The verdicts, into the file and to stdout, and the exit code a CI step can gate on first — the
# text grep is the second net. `emit_summary` is defined near the top because the port refusal
# above exits early and still has to leave a file a machine can read.
emit_summary
exit $?
