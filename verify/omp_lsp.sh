#!/usr/bin/env bash
# Editor-agnostic proof: jev-lsp driven by OMP, a client that shares no code with this repo.
#
#   bash verify/omp_lsp.sh [<server-bin-name>]
#
# OMP is a third LSP client — not Neovim, not the spec-derived `verify/lsp_client.py` — so a
# finding that reaches its `lsp` tool is evidence the standard surfaces are enough. The fixture
# registers `target/release/<bin>` in its *own* `<fixture>/.omp/lsp.json` (OMP merges that cwd
# config; nothing is written under `~/.omp` or the repository's `.omp/`), and the agent is asked
# to drive the `lsp` tool over that fixture.
#
# Checks, in order, per fixture:
#   * the agent called the `lsp` tool for `handler.rs` diagnostics at all;
#   * `jev.inspect` is reachable from OMP's tool surface (`action=request`,
#     `query=workspace/executeCommand`) and answers with the finding;
#   * the rule finding reaches OMP's `diagnostics` result: the rule title, the judgement that
#     followed it, the `.unwrap()` line, and the finding id;
#   * negative control: the same fixture with no `.jev/rules/` produces no jev diagnostic, and
#     its `jev.inspect` says nothing was run (`no_rules`) — so a green run cannot be an artifact
#     of the harness finding something else. This control means **"nothing applies to this
#     file"**, and it relies on the *shipped* defaults not claiming `**/*.rs` (they claim
#     `**/*.py`, `**/*.java` and `**/*.md` as of 2026-09-20; PROTOCOL §9). A shipped rule about
#     Rust turns this into "the shipped rules found nothing on this file" rather than
#     `no_rules`, and the check has to be rewritten with it.
#
# The decision tier is the stub (`JEV_DECIDE_BASE_URL`/`JEV_DECIDE_MODEL`), so nothing reaches
# the network and no API key is required. `omp` unavailable, no model, or an agent that never
# drives the tool is a SKIP with the reason — never a false ok. Bounded waits only.
set -u

HERE="$(cd "$(dirname "$(realpath "$0")")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
BIN_NAME="${1:-jev-lsp}"
SERVER="$REPO/target/release/$BIN_NAME"
PORT=8099
ORIGIN="http://127.0.0.1:$PORT"
OUT="${OMP_LSP_OUT:-/tmp/w2-omp.log}"

: > "$OUT"

STUB_PID=""
cleanup() {
  if [ -n "$STUB_PID" ]; then
    kill "$STUB_PID" 2>/dev/null
    wait "$STUB_PID" 2>/dev/null
  fi
}
trap cleanup EXIT

# --- environment ---------------------------------------------------------------------------------

if ! command -v omp >/dev/null 2>&1; then
  {
    echo "SKIP  omp is not on PATH, so a non-Neovim client cannot be driven"
    echo "[omp] 0 failure(s), 1 skip(s)"
  } | tee -a "$OUT"
  exit 0
fi
if [ ! -x "$SERVER" ]; then
  {
    echo "SKIP  $SERVER is not executable (build it first)"
    echo "[omp] 0 failure(s), 1 skip(s)"
  } | tee -a "$OUT"
  exit 0
fi

# A supervised stub. If one already answers /health it belongs to the caller (the suite runner
# holds one for the whole table) — reuse it and leave it alone.
if curl -fsS -o /dev/null --max-time 1 "$ORIGIN/health" 2>/dev/null; then
  printf 'ok    reusing the stub already answering %s/health\n' "$ORIGIN" | tee -a "$OUT"
else
  ( cd "$REPO" && python3 verify/stub_model.py ) >>"$OUT" 2>&1 &
  STUB_PID=$!
  for _ in $(seq 1 50); do
    if curl -fsS -o /dev/null --max-time 1 "$ORIGIN/health" 2>/dev/null; then break; fi
    sleep 0.2
  done
  if ! curl -fsS -o /dev/null --max-time 1 "$ORIGIN/health" 2>/dev/null; then
    {
      echo "SKIP  the stub never answered $ORIGIN/health"
      echo "[omp] 0 failure(s), 1 skip(s)"
    } | tee -a "$OUT"
    exit 0
  fi
  printf 'ok    stub up on %s\n' "$ORIGIN" | tee -a "$OUT"
fi

export JEV_LSP_BIN="$SERVER"
export JEV_BASE_URL="$ORIGIN/v1" JEV_MODEL=stub-model
export JEV_DECIDE_BASE_URL="$ORIGIN/v1" JEV_DECIDE_MODEL=stub-model
# The shared LSP mux would need a broker; a private stdio process is what this harness is about.
export PI_DISABLE_LSPMUX=1

RULES='{"schema":"jev.rules/1","rules":[{"id":"no-unwrap-in-handlers","title":"Unwrap in a request handler","text":"A handler must not unwrap; return the error instead.","severity":"warning","applies_to":["**/*.rs"],"inspection":{"kind":"regex","pattern":"\\.unwrap\\(\\)","max_matches":0},"judgement":{"question":"Is this unwrap reachable from a request handler?","criteria":{"true":"the call sits on a path a request can reach","false":"the call is in a test, a startup path, or behind an invariant"},"min_probability":0.75},"verb_hint":"fix"}]}'
TITLE='Unwrap in a request handler'

# --- fixture -------------------------------------------------------------------------------------

make_fixture() { # $1 = dir, $2 = with_rules (1/0)
  local dir="$1" with_rules="$2"
  mkdir -p "$dir/.git" "$dir/.omp"
  printf 'use std::fs;\n\npub fn handle(path: &str) -> String {\n    let body = fs::read_to_string(path).unwrap();\n    body\n}\n' > "$dir/handler.rs"
  if [ "$with_rules" = 1 ]; then
    mkdir -p "$dir/.jev/rules"
    printf '%s\n' "$RULES" > "$dir/.jev/rules/example.json"
  fi
  # The fixture's own cwd config. `command` is absolute; `args`/`fileTypes`/`rootMarkers` are the
  # three a genuinely new server needs (omp://lsp-config.md).
  cat > "$dir/.omp/lsp.json" <<JSON
{"servers":{"jev-lsp":{"command":"$SERVER","args":["--stdio"],"fileTypes":[".rs"],"rootMarkers":[".git"]}}}
JSON
}

PROMPT='Call the lsp tool twice, in this exact order, and no other tool.
1. action=request, file=handler.rs, query=workspace/executeCommand, payload=%s
2. action=diagnostics, file=handler.rs
Then print the second result verbatim.'
run_omp() { # $1 = fixture dir, $2 = jsonl path, $3 = stderr path
  local dir="$1" jsonl="$2" err="$3"
  local abs="$dir/handler.rs"
  local payload prompt
  payload=$(printf '{"command":"jev.inspect","arguments":[{"path":"%s","force":true}]}' "$abs")
  # The payload is quoted in the prompt so the agent passes it as the tool's JSON string.
  prompt=$(printf "$PROMPT" "'$payload'")
  ( cd "$dir" && timeout 300 omp -p --mode json --cwd "$dir" --auto-approve --no-session \
      --tools lsp --max-time 240 "$prompt" ) >"$jsonl" 2>"$err"
}

# --- one fixture's run, parsed and checked ------------------------------------------------------

check_fixture() { # $1 = dir, $2 = mode (positive|negative), $3 = jsonl, $4 = stderr
  local dir="$1" mode="$2" jsonl="$3" err="$4"
  python3 - "$jsonl" "$mode" "$TITLE" "$err" <<'PY'
import json, sys

path, mode, title, errpath = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
try:
    lines = open(path, encoding='utf-8', errors='replace').read().splitlines()
except OSError:
    lines = []

starts, ends, order = {}, {}, []
for line in lines:
    line = line.strip()
    if not line.startswith('{'):
        continue
    try:
        ev = json.loads(line)
    except ValueError:
        continue
    if ev.get('toolName') != 'lsp':
        continue
    if ev.get('type') == 'tool_execution_start':
        starts[ev.get('toolCallId')] = ev.get('args') or {}
        order.append(ev.get('toolCallId'))
    elif ev.get('type') == 'tool_execution_end':
        res = ev.get('result')
        if isinstance(res, dict):
            ends[ev.get('toolCallId')] = '\n'.join(c.get('text', '') for c in res.get('content', []))
        else:
            ends[ev.get('toolCallId')] = json.dumps(res)

def call(action, query=None):
    for cid in order:
        args = starts[cid]
        if args.get('action') != action:
            continue
        if query is not None and args.get('query') != query:
            continue
        return args, ends.get(cid, '')
    return None, None

fails = skips = 0
def ok(label):
    print('ok    ' + label)
def fail(label, detail=''):
    global fails
    fails += 1
    print('FAIL  ' + label + (('  — ' + str(detail)) if detail != '' else ''))
def skip(label, detail=''):
    global skips
    skips += 1
    print('SKIP  ' + label + (('  — ' + str(detail)) if detail != '' else ''))

diag_args, diag_text = call('diagnostics')
if diag_args is None:
    why = ''
    try:
        why = open(errpath, encoding='utf-8', errors='replace').read().strip().splitlines()[-1]
    except OSError:
        pass
    skip(
        'the agent drove the lsp tool for handler.rs diagnostics',
        'no lsp diagnostics call in the stream' + (('; omp stderr: ' + why) if why else ''),
    )
    sys.exit(0)

ok('the agent drove the lsp tool for handler.rs diagnostics')

# The inspect call's result, parsed out of OMP's rendering.
def payload_json(text):
    if text is None:
        return None
    body = text.split(':\n', 1)[1] if ':\n' in text else text
    try:
        return json.loads(body)
    except ValueError:
        return None

request_args, request_text = call('request', 'workspace/executeCommand')
inspect = payload_json(request_text)
findings = (inspect or {}).get('findings') or []
if inspect is None or inspect.get('ok') is not True:
    fail('jev.inspect is reachable from OMP\'s tool surface (workspace/executeCommand)',
         (request_text or 'the call did not happen')[:400])
else:
    ok('jev.inspect is reachable from OMP\'s tool surface (workspace/executeCommand)')

if mode == 'positive':
    if not findings:
        fail('the rule finding reaches OMP\'s diagnostics', 'jev.inspect returned no finding')
    else:
        f = findings[0]
        line = (f.get('line') or 0) + 1
        missing = []
        for what, needle in (
            ('the rule title', title),
            ('the finding id', str(f.get('id'))),
            ('the judgement', str(f.get('detail') or '')),
            ('the sign column source', '[jev]'),
            ('the .unwrap() line', '%d:' % line),
        ):
            if needle and needle not in (diag_text or ''):
                missing.append(what)
        if missing:
            fail('the rule finding reaches OMP\'s diagnostics', 'missing ' + ', '.join(missing) + ' in: ' + (diag_text or '')[:400])
        else:
            ok('the rule finding reaches OMP\'s diagnostics: title, judgement, .unwrap() line, finding id')
        if 'warning' in (diag_text or ''):
            ok('it arrives with the rule\'s severity')
        else:
            fail('it arrives with the rule\'s severity', (diag_text or '')[:200])
else:
    codes = [s.get('code') for s in (inspect or {}).get('skipped') or []]
    if findings:
        fail('the negative control has nothing to run', 'jev.inspect still returned a finding')
    elif 'no_rules' not in codes:
        fail('the negative control has nothing to run', 'skipped was %r, expected no_rules' % (codes,))
    else:
        ok('the negative control has nothing to run (no_rules, no finding)')
    leaked = []
    if title in (diag_text or ''):
        leaked.append('the rule title')
    if '[jev]' in (diag_text or ''):
        leaked.append('a jev diagnostic')
    if leaked:
        fail('no jev diagnostic reaches OMP without a rule', 'the result carried ' + ', '.join(leaked) + ': ' + (diag_text or '')[:300])
    else:
        ok('no jev diagnostic reaches OMP without a rule')

for cid in order:
    text = ends.get(cid, '') or ''
    if 'Method not found' in text:
        print('note  OMP asked for something jev-lsp does not serve: ' + ' '.join(text.split())[:160])
        break

sys.exit(1 if fails else 0)
PY
}

# --- run -----------------------------------------------------------------------------------------

A_DIR="$(mktemp -d /tmp/jev-omp-rules.XXXXXX)"
B_DIR="$(mktemp -d /tmp/jev-omp-norules.XXXXXX)"
make_fixture "$A_DIR" 1
make_fixture "$B_DIR" 0

# The helper's exit status is the verdict; `| tee` would throw it away, so it is captured with
# PIPESTATUS at each call site. `set -o pipefail` would change every other pipeline here at once,
# and several of them (`grep -c … || true`) are expected to be non-zero.
check_status=0
printf '\n----- fixture: rules present (%s)\n' "$A_DIR" | tee -a "$OUT"
run_omp "$A_DIR" /tmp/omp-rules.jsonl /tmp/omp-rules.err
grep -E '"tool_execution_(start|end)"' /tmp/omp-rules.jsonl >>"$OUT" || true
check_fixture "$A_DIR" positive /tmp/omp-rules.jsonl /tmp/omp-rules.err | tee -a "$OUT"
code=${PIPESTATUS[0]}
[ "$code" -eq 0 ] || check_status=$code

printf '\n----- fixture: no rules (negative control) (%s)\n' "$B_DIR" | tee -a "$OUT"
run_omp "$B_DIR" /tmp/omp-norules.jsonl /tmp/omp-norules.err
grep -E '"tool_execution_(start|end)"' /tmp/omp-norules.jsonl >>"$OUT" || true
check_fixture "$B_DIR" negative /tmp/omp-norules.jsonl /tmp/omp-norules.err | tee -a "$OUT"
code=${PIPESTATUS[0]}
[ "$code" -eq 0 ] || check_status=$code

rm -rf "$A_DIR" "$B_DIR"

FAILS=$(grep -cE '^FAIL' "$OUT" || true)
SKIPS=$(grep -cE '^SKIP' "$OUT" || true)
printf '\n[omp] %s failure(s), %s skip(s)\n' "$FAILS" "$SKIPS" | tee -a "$OUT"
[ "$FAILS" -eq 0 ] || exit 1
[ "$check_status" -eq 0 ] || exit "$check_status"
exit 0
