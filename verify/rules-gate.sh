#!/usr/bin/env bash
# verify/rules-gate.sh — the repository's rules, run over the files a session is about to commit.
#
# Why this exists: a CLI agent never sees a diagnostic. The rules in `.jev/rules/` are only in
# the loop if something runs them over a diff and fails when one fires, and that is this script.
# It is the same pass the editor runs — the shipped `jev inspect --force`, which shares
# `jev-core` with the language server, so a gate finding and a sign-column finding cannot
# disagree about what a rule says.
#
#   bash verify/rules-gate.sh                 # the files this session is about to commit
#   bash verify/rules-gate.sh --json          # the same, as one JSON object on stdout
#   bash verify/rules-gate.sh --quiet         # findings only, no counts
#   bash verify/rules-gate.sh <path> [<path>] # named paths instead of the diff
#
# Reads:
#   `.jev/rules/*.json` behind the paths' git root, through `target/release/jev inspect --force`
#   the path list: `git diff --name-only origin/main` plus untracked files, when no path is given
#   `JEV_DECIDE_WIRE` (default `system_one`), `JEV_DECIDE_BASE_URL` (default
#   `https://opencode.ai/zen/v1`), `JEV_DECIDE_MODEL` (default `jev-1.13`),
#   `JEV_DECIDE_TIMEOUT_MS` (default 15000)
#   the decide key: `$TYPESAFE_API_KEY`, else the file in `$JEV_GATE_KEY_FILE`, else
#   `~/.omp/agent/opencode.key`. The key is read, exported to the child, and never printed.
#
# Exit codes — the middle one is the point, because a gate that cannot run must never look like
# a pass:
#   0  the pass ran and no finding cleared its floor
#   1  the pass ran and at least one finding did
#   2  the pass could not run: no binary, no key, or every path that had a candidate failed at
#      the transport. A path with no candidate asks the model nothing, so it does not make a dead
#      endpoint visible; the reason is on stderr either way, so CI can tell "clean" from "never
#      checked".
set -u

HERE="$(cd "$(dirname "$(realpath "$0")")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
BIN="${JEV_GATE_BIN:-$REPO/target/release/jev}"
KEY_FILE="${JEV_GATE_KEY_FILE:-$HOME/.omp/agent/opencode.key}"
export JEV_DECIDE_WIRE="${JEV_DECIDE_WIRE:-system_one}"
export JEV_DECIDE_BASE_URL="${JEV_DECIDE_BASE_URL:-https://opencode.ai/zen/v1}"
export JEV_DECIDE_MODEL="${JEV_DECIDE_MODEL:-jev-1.13}"
export JEV_DECIDE_TIMEOUT_MS="${JEV_DECIDE_TIMEOUT_MS:-15000}"

QUIET=0
JSON=0
PATHS=()
for arg in "$@"; do
  case "$arg" in
    --quiet | -q) QUIET=1 ;;
    --json) JSON=1 ;;
    --help | -h)
      sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      PATHS+=("$arg")
      ;;
  esac
done

die() { # $1 = why the gate cannot run
  printf 'rules-gate: %s\n' "$1" >&2
  if [ "$JSON" = 1 ]; then printf '{"schema":"jev.gate/1","ran":false,"reason":%s}\n' "$(python3 -c 'import json,sys;print(json.dumps(sys.argv[1]))' "$1")"; fi
  exit 2
}

[ -x "$BIN" ] || die "$BIN is not executable (cargo build --release)"

# The key name is the decide tier's `api_key_env`; no environment override for the name exists,
# which is why the value is exported under the default name rather than read under its own.
if [ -n "${TYPESAFE_API_KEY:-}" ]; then
  :
elif [ -r "$KEY_FILE" ]; then
  TYPESAFE_API_KEY="$(cat "$KEY_FILE")"
else
  die "no decide key: set TYPESAFE_API_KEY or write ${KEY_FILE} (0666 → 0600)"
fi
export TYPESAFE_API_KEY

if [ "${#PATHS[@]}" -eq 0 ]; then
  base="origin/main"
  git -C "$REPO" rev-parse --verify --quiet "$base" >/dev/null 2>&1 || base="HEAD"
  {
    git -C "$REPO" diff --name-only "$base" 2>/dev/null
    git -C "$REPO" ls-files --others --exclude-standard 2>/dev/null
  } | sort -u >"${TMPDIR:-/tmp}/rules-gate-paths.$$"
  while IFS= read -r rel; do
    [ -n "$rel" ] && [ -f "$REPO/$rel" ] && PATHS+=("$REPO/$rel")
  done <"${TMPDIR:-/tmp}/rules-gate-paths.$$"
  rm -f "${TMPDIR:-/tmp}/rules-gate-paths.$$"
fi

if [ "${#PATHS[@]}" -eq 0 ]; then
  if [ "$JSON" = 1 ]; then printf '{"schema":"jev.gate/1","ran":true,"files":0,"findings":[]}\n'; fi
  [ "$QUIET" = 1 ] || printf 'rules-gate: nothing to check\n'
  exit 0
fi

RESULTS="${TMPDIR:-/tmp}/rules-gate-results.$$"
: >"$RESULTS"
for path in "${PATHS[@]}"; do
  case "$path" in
    /*) abs="$path" ;;
    *) if [ -f "$path" ]; then abs="$(cd "$(dirname "$path")" && pwd)/$(basename "$path")"; else abs="$REPO/$path"; fi ;;
  esac
  [ -f "$abs" ] || continue
  # One artifact per line. The CLI's own line ends with a newline, so the closing brace is
  # appended to the captured text rather than printed after it.
  # A non-zero exit is reported by the artifact itself (transport_error), so the status is kept
  # rather than discarded: swallowing it here is the failure this repository's own rules flag.
  out="$("$BIN" inspect --force "$abs" 2>/dev/null)"
  status=$?
  if [ -z "$out" ]; then
    out="{\"ok\":false,\"error\":{\"code\":\"no_output\",\"message\":\"exit $status\"}}"
  fi
  printf '{"path":%s,"result":%s}\n' \
    "$(python3 -c 'import json,sys;print(json.dumps(sys.argv[1]))' "$abs")" "$out" >>"$RESULTS"
done

python3 - "$RESULTS" "$QUIET" "$JSON" <<'PY'
import json, sys

path_file, quiet, as_json = sys.argv[1], sys.argv[2] == "1", sys.argv[3] == "1"
findings, ran, failed, considered, candidates = [], 0, 0, 0, 0
skips = {}
for line in open(path_file, encoding="utf-8", errors="replace"):
    line = line.strip()
    if not line.startswith("{"):
        continue
    try:
        row = json.loads(line)
    except json.JSONDecodeError:
        continue
    body = row.get("result") or {}
    if body.get("error"):
        failed += 1
        skips[body["error"].get("code", "error")] = skips.get(body["error"].get("code", "error"), 0) + 1
        continue
    ran += 1
    considered += body.get("considered") or 0
    candidates += body.get("candidates") or 0
    for s in body.get("skipped", []):
        code = s.get("code", "skipped")
        skips[code] = skips.get(code, 0) + 1
    for f in body.get("findings", []):
        detail = f.get("detail", "")
        p = detail.rsplit("(p=", 1)[-1].rstrip(")") if "(p=" in detail else "?"
        findings.append({"path": row["path"], "line": f["line"] + 1, "label": f["label"],
                         "probability": p, "id": f["id"], "verb": f.get("verb")})

if ran == 0:
    reason = "the decide endpoint did not answer for any path that had a candidate"
    if as_json:
        print(json.dumps({"schema": "jev.gate/1", "ran": False, "reason": reason, "failures": skips}))
    print("rules-gate: %s (%s)" % (reason, ", ".join("%s×%d" % (k, v) for k, v in skips.items())), file=sys.stderr)
    sys.exit(2)

if as_json:
    print(json.dumps({"schema": "jev.gate/1", "ran": True, "files": ran, "considered": considered,
                      "candidates": candidates, "skipped": skips, "findings": findings}, indent=1))
else:
    for f in findings:
        print("%s:%d  %s  (p=%s)" % (f["path"], f["line"], f["label"], f["probability"]))
    if not quiet:
        print("rules-gate: %d file(s), %d rule(s) considered, %d candidate(s), %d finding(s), %d skipped"
              % (ran, considered, candidates, len(findings), sum(skips.values())))
        if failed:
            print("rules-gate: %d path(s) did not answer (%s)" % (failed, ", ".join(skips)))

sys.exit(1 if findings else 0)
PY
rc=$?
rm -f "$RESULTS"
exit $rc
