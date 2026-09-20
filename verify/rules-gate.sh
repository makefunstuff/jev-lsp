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
#   bash verify/rules-gate.sh --all           # every tracked file (plus untracked)
#   bash verify/rules-gate.sh --json          # one JSON object on stdout
#   bash verify/rules-gate.sh --quiet         # findings only, no counts
#   bash verify/rules-gate.sh <path> [<path>] # named paths instead of the diff
#
# A scope has to be real. With no path and no `--all`, the scope is
# `git diff --name-only origin/main` plus untracked files; a scope that turns out to be empty is
# **exit 2**, not a pass — a gate that scanned nothing has not checked anything, and "nothing was
# scanned" must not look like "the rules ran and were quiet". CI should pass an explicit scope
# (the branch diff, or `--all`), and a person who asked the gate to check nothing gets 2.
#
# Reads:
#   `.jev/rules/*.json` behind the paths' git root, through `target/release/jev inspect --force`
#   `JEV_DECIDE_WIRE` (default `system_one`), `JEV_DECIDE_BASE_URL` (default
#   `https://opencode.ai/zen/v1`), `JEV_DECIDE_MODEL` (default `jev-1.13`),
#   `JEV_DECIDE_TIMEOUT_MS` (default 15000)
#   the decide key: `$TYPESAFE_API_KEY`, else the file in `$JEV_GATE_KEY_FILE`, else
#   `~/.omp/agent/opencode.key`. The key is read, exported to the child, and never printed.
#
# Exit codes:
#   0  files were scanned and no finding cleared its floor
#   1  files were scanned and at least one finding did
#   2  the gate could not do its job: no binary, no key, every path that had a candidate failing
#      at the transport, or **nothing scanned** — an empty scope, or a scope whose every path was
#      missing or binary. The reason is printed on stderr as well as stdout, so CI can tell
#      "clean" from "never checked" from "checked nothing".
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
ALL=0
PATHS=()
for arg in "$@"; do
  case "$arg" in
    --quiet | -q) QUIET=1 ;;
    --json) JSON=1 ;;
    --all) ALL=1 ;;
    --help | -h)
      sed -n '2,34p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      PATHS+=("$arg")
      ;;
  esac
done

json_string() { python3 -c 'import json,sys;print(json.dumps(sys.argv[1]))' "$1"; }

die() { # $1 = why the gate cannot run
  if [ "$JSON" = 1 ]; then
    printf '{"schema":"jev.gate/1","ran":false,"reason":%s}\n' "$(json_string "$1")"
  fi
  printf 'rules-gate: %s\n' "$1" >&2
  exit 2
}

# Nothing to scan is a failure to do the job, not a clean run. No path is ever implied: the
# message names which scope came up empty, because "the diff was empty" and "you pointed me at
# nothing" are different mistakes.
nothing_scanned() { # $1 = which scope was empty
  local msg="nothing was scanned: $1"
  if [ "$JSON" = 1 ]; then
    printf '{"schema":"jev.gate/1","ran":false,"scanned":0,"reason":%s}\n' "$(json_string "$msg")"
  else
    printf 'rules-gate: %s\n' "$msg"
  fi
  printf 'rules-gate: %s\n' "$msg" >&2
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
  die "no decide key: set TYPESAFE_API_KEY or write ${KEY_FILE} (chmod 600)"
fi
export TYPESAFE_API_KEY

LIST="${TMPDIR:-/tmp}/rules-gate-paths.$$"
EMPTY_SCOPE="none of the given paths is a readable file"

if [ "$ALL" = 1 ]; then
  EMPTY_SCOPE="--all found no tracked or untracked file under $REPO"
  {
    git -C "$REPO" ls-files 2>/dev/null
    git -C "$REPO" ls-files --others --exclude-standard 2>/dev/null
  } | sort -u >"$LIST"
elif [ "${#PATHS[@]}" -eq 0 ]; then
  base="origin/main"
  git -C "$REPO" rev-parse --verify --quiet "$base" >/dev/null 2>&1 || base="HEAD"
  EMPTY_SCOPE="no file differs from $base and none is untracked (pass --all to scan the tree, or name paths)"
  {
    git -C "$REPO" diff --name-only "$base" 2>/dev/null
    git -C "$REPO" ls-files --others --exclude-standard 2>/dev/null
  } | sort -u >"$LIST"
else
  : >"$LIST"
fi

if [ -s "$LIST" ]; then
  while IFS= read -r rel; do
    [ -n "$rel" ] && [ -f "$REPO/$rel" ] && PATHS+=("$REPO/$rel")
  done <"$LIST"
fi
rm -f "$LIST"

[ "${#PATHS[@]}" -gt 0 ] || nothing_scanned "$EMPTY_SCOPE"

RESULTS="${TMPDIR:-/tmp}/rules-gate-results.$$"
: >"$RESULTS"
scanned=0
binary=0
missing=0
for path in "${PATHS[@]}"; do
  case "$path" in
    /*) abs="$path" ;;
    *) if [ -f "$path" ]; then abs="$(cd "$(dirname "$path")" && pwd)/$(basename "$path")"; else abs="$REPO/$path"; fi ;;
  esac
  if [ ! -f "$abs" ]; then
    missing=$((missing + 1))
    continue
  fi
  # A path the pass cannot read is named and skipped, by the same cheap signal the engine uses
  # (`gates::is_binary`: a NUL byte in the first 8 KiB). Counting a tarball as "did not answer"
  # would put a build artifact in the same bucket as a dead endpoint.
  if ! python3 -c 'import sys; sys.exit(1 if b"\x00" in open(sys.argv[1], "rb").read(8192) else 0)' "$abs"; then
    binary=$((binary + 1))
    if [ "$QUIET" != 1 ] && [ "$JSON" != 1 ]; then
      printf 'rules-gate: skipped  %s  (binary)\n' "$abs"
    fi
    continue
  fi
  # One artifact per line. The CLI's own line ends with a newline, so the closing brace is
  # appended to the captured text rather than printed after it. A non-zero exit is reported by
  # the artifact itself (transport_error), so the status is kept rather than discarded:
  # swallowing it here is the failure this repository's own rules flag.
  scanned=$((scanned + 1))
  out="$("$BIN" inspect --force "$abs" 2>/dev/null)"
  status=$?
  if [ -z "$out" ]; then
    out="{\"ok\":false,\"error\":{\"code\":\"no_output\",\"message\":\"exit $status\"}}"
  fi
  printf '{"path":%s,"result":%s}\n' "$(json_string "$abs")" "$out" >>"$RESULTS"
done

# Paths that were skipped are not a scan. Saying "0 findings" over them would be the same lie as
# an empty scope, one path at a time.
if [ "$scanned" -eq 0 ]; then
  detail="no readable text file among ${#PATHS[@]} path(s)"
  [ "$binary" -eq 0 ] || detail="$detail, $binary binary"
  [ "$missing" -eq 0 ] || detail="$detail, $missing missing"
  rm -f "$RESULTS"
  nothing_scanned "$detail"
fi

python3 - "$RESULTS" "$QUIET" "$JSON" <<'PY'
import json, sys

path_file, quiet, as_json = sys.argv[1], sys.argv[2] == "1", sys.argv[3] == "1"
findings, ran, failed, considered, candidates = [], 0, 0, 0, 0
skips, failures = {}, {}
unchecked = []
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
        # A failure of the pass itself, kept apart from the skip codes: `no_rules` is a run that
        # happened and found nothing to ask, and must never be reported as "did not answer".
        failed += 1
        code = body["error"].get("code", "error")
        failures[code] = failures.get(code, 0) + 1
        unchecked.append((row["path"], code))
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
        print(json.dumps({"schema": "jev.gate/1", "ran": False, "scanned": 0, "reason": reason,
                          "failures": failures}))
    print("rules-gate: %s (%s)" % (reason, ", ".join("%s×%d" % (k, v) for k, v in failures.items())),
          file=sys.stderr)
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
        # A path the pass could not read is named, not counted: a file that went unchecked is
        # the same failure as a run that never happened, one path at a time.
        for name, code in unchecked[:10]:
            print("rules-gate: unchecked  %s  (%s)" % (name, code))
        if failed:
            print("rules-gate: %d path(s) did not answer (%s)"
                  % (failed, ", ".join("%s×%d" % (k, v) for k, v in failures.items())))

sys.exit(1 if findings else 0)
PY
rc=$?
rm -f "$RESULTS"
exit $rc
