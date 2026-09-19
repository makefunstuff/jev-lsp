#!/usr/bin/env python3
"""Soak a real model across languages.

The six runs behind the last set of fixes were one language on one model, and every defect
found that way lived in language-specific shape — a re-indented Python block, a one-line
statement answered with a block. This drives several languages through the whole loop and
reports what actually happened, per run, without asserting stub-shaped expectations.

For each fixture it asks the same three questions:

  * did the ambient pass produce a finding,
  * did a code action resolve to an edit,
  * and does the file still parse afterwards?

The last one is the point. An edit that leaves a file unparseable is worse than no edit, and
no unit test can see it.

    python3 verify/soak.py --base-url http://127.0.0.1:4000/v1 --model deepseek/deepseek-flash \\
        --rounds 2 [--bin target/release/jev-lsp] [--only python,rust]

    # a hosted endpoint that wants a key: JEV_API_KEY_ENV names the variable holding it
    JEV_API_KEY_ENV=OPENROUTER_API_KEY python3 verify/soak.py \\
        --base-url https://openrouter.ai/api/v1 --model google/gemini-2.5-flash-lite --rounds 2

The ambient pass measured here is the *chat review*: `rules.enabled` is off in each run's
settings, because the ambient pass is the rules pass while rules are on and a scratch workspace
has no `.jev/rules` to run. If no run reached a model, the soak says so and exits non-zero — its
"0 edits, 0 broken" is otherwise the shape of a clean run.

Exits 0 unless a *parsed* file was left broken, or no model was reached at all.
"""
import argparse
import ast
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
sys.path.insert(0, HERE)

from smoke import Lsp  # noqa: E402

# Each fixture carries a plausible defect for the review pass to find.
FIXTURES = {
    "python": ("loader.py", '''import json


def load_first_name(path):
    f = open(path)
    data = json.load(f)
    return data["items"][0]["name"]
'''),
    "rust": ("retry.rs", '''use std::fs::File;

pub fn read_config(path: &str) -> String {
    let f = File::open(path).unwrap();
    let mut s = String::new();
    std::io::Read::read_to_string(&mut f, &mut s);
    s
}
'''),
    "go": ("store.go", '''package store

import "os"

func Load(path string) []byte {
    data, _ := os.ReadFile(path)
    return data
}
'''),
    "typescript": ("api.ts", '''export async function fetchUser(id: string) {
  const res = await fetch(`/api/users/${id}`);
  const body = await res.json();
  return body.user.name;
}
'''),
    "markdown": ("NOTES.md", '''# Deploy notes

Run the migration first, then restart the service. The rollback path has not been
tested since the schema change.
'''),
    "unknown": ("mystery.conf", '''[service]
retries = 3
timeout = 0
path = /var/lib/data
'''),
}

# What "still parses" means where a checker exists. Absent means the check is skipped,
# which is reported rather than counted as a pass.
PARSERS = {
    "python": lambda text: ast.parse(text),
}


def parses(language, text):
    checker = PARSERS.get(language)
    if checker is None:
        return None
    try:
        checker(text)
        return True
    except SyntaxError:
        return False


def run_once(binary, language, base_url, model, workdir, timeout):
    name, content = FIXTURES[language]
    path = os.path.join(workdir, name)
    with open(path, "w") as fh:
        fh.write(content)
    uri = "file://" + path

    env = dict(os.environ, JEV_BASE_URL=base_url, JEV_MODEL=model, JEV_REVIEW_MODEL=model)
    server = Lsp([binary, "--stdio"], env)
    # The ambient pass is the rules pass while `rules.enabled` is at its default, and every
    # scratch workspace here has no `.jev/rules` — so with rules on, the save below measures a
    # pass with nothing to run and the review tier is never asked. This soak is about the loop's
    # edits, so the ambient slot is handed to the chat review explicitly.
    server.settings = {"rules": {"enabled": False}}
    result = {"language": language, "findings": 0, "edit": False, "parses": None,
              "ambient_ms": None, "resolve_ms": None, "tokens": 0, "note": ""}
    try:
        server.request("initialize", {
            "processId": os.getpid(), "rootUri": "file://" + workdir,
            "capabilities": {"workspace": {"configuration": True}},
        })
        server.notify("initialized", {})
        server.notify("textDocument/didOpen", {"textDocument": {
            "uri": uri, "languageId": language if language != "unknown" else "",
            "version": 1, "text": content}})

        started = time.time()
        server.notify("textDocument/didSave", {"textDocument": {"uri": uri}})
        items, refreshes = [], 0
        while time.time() - started < timeout:
            seen = len(server.saw_request("workspace/diagnostic/refresh"))
            if seen > refreshes:
                refreshes = seen
                report = server.request("textDocument/diagnostic",
                                        {"textDocument": {"uri": uri}}).get("result", {})
                items = report.get("items") or (
                    report.get("fullDocumentDiagnosticReport", {}) or {}
                ).get("items", [])
                if items:
                    break
            time.sleep(0.05)
        result["ambient_ms"] = int((time.time() - started) * 1000)
        result["findings"] = len(items)
        # Endpoint-reported, so a run that never reached a model is distinguishable from one the
        # model answered with nothing. Read after the ambient pass, which is now the chat review.
        status = server.request("workspace/executeCommand",
                                {"command": "jev.status", "arguments": []}).get("result", {})
        result["tokens"] = (status.get("budget", {}) or {}).get("tokens_used", 0)

        actions = server.request("textDocument/codeAction", {
            "textDocument": {"uri": uri},
            "range": {"start": {"line": 2, "character": 4}, "end": {"line": 2, "character": 4}},
            "context": {"triggerKind": 1, "diagnostics": []},
        }, timeout=30).get("result") or []
        pick = next((a for a in actions if a["kind"] == "quickfix.jev"), None) \
            or next((a for a in actions if a["kind"] == "refactor.rewrite.jev"), None)
        if pick is None:
            result["note"] = "no action to resolve"
            return result

        started = time.time()
        resolved = server.request("codeAction/resolve", pick, timeout=timeout + 30).get("result", {})
        result["resolve_ms"] = int((time.time() - started) * 1000)
        edit = resolved.get("edit")
        if edit is None:
            result["note"] = (resolved.get("disabled") or {}).get("reason", "no edit")[:80]
            return result

        if not server.apply_edit(edit)[0]:
            result["note"] = "the client refused the edit"
            return result
        result["edit"] = True
        with open(path) as fh:
            result["parses"] = parses(language, fh.read())
        return result
    finally:
        server.stop()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default=os.path.join(REPO, "target", "release", "jev-lsp"))
    ap.add_argument("--base-url", default=os.environ.get("JEV_BASE_URL", ""))
    ap.add_argument("--model", default=os.environ.get("JEV_MODEL", ""))
    ap.add_argument("--rounds", type=int, default=1)
    ap.add_argument("--timeout", type=float, default=60.0)
    ap.add_argument("--only", default="")
    args = ap.parse_args()

    if not args.base_url or not args.model:
        print("soak: --base-url and --model are required (this drives a real endpoint)",
              file=sys.stderr)
        return 2

    languages = [l.strip() for l in args.only.split(",") if l.strip()] or list(FIXTURES)
    print(f"endpoint: {args.base_url}\nmodel   : {args.model}\nrounds  : {args.rounds}\n")

    rows, broken = [], 0
    for round_no in range(1, args.rounds + 1):
        for language in languages:
            workdir = tempfile.mkdtemp(prefix=f"jev-soak-{language}-")
            try:
                r = run_once(args.bin, language, args.base_url, args.model, workdir, args.timeout)
            except Exception as e:  # a harness failure is a result too
                r = {"language": language, "findings": 0, "edit": False, "parses": None,
                     "ambient_ms": None, "resolve_ms": None, "note": f"harness: {e}"[:80]}
            finally:
                shutil.rmtree(workdir, ignore_errors=True)
            r["round"] = round_no
            rows.append(r)
            verdict = {True: "parses", False: "BROKEN", None: "n/a"}[r["parses"]]
            if r["parses"] is False:
                broken += 1
            print(f"  r{round_no} {r['language']:<10} findings={r['findings']} "
                  f"ambient={r['ambient_ms']}ms edit={'yes' if r['edit'] else 'no '} "
                  f"resolve={r['resolve_ms']}ms {verdict:<7} {r['note']}")

    edits = sum(1 for r in rows if r["edit"])
    print(f"\n[soak] {edits}/{len(rows)} runs produced an applied edit; "
          f"{broken} left the file unparseable")
    answered = [r["tokens"] for r in rows if r.get("tokens")]
    print(f"[soak] the endpoint billed tokens in {len(answered)}/{len(rows)} runs")
    if rows and not answered:
        # A soak in which no model answered measured nothing: its "0 edits, 0 broken" is the
        # shape of a clean run, which is the one thing it must never be mistaken for.
        print(f"[soak] no model answered — {rows[-1].get('note') or '(no reason recorded)'}")
        return 1
    if rows:
        amb = [r["ambient_ms"] for r in rows if r["ambient_ms"]]
        res = [r["resolve_ms"] for r in rows if r["resolve_ms"]]
        if amb:
            print(f"[soak] ambient  min/median/max: {min(amb)}/{sorted(amb)[len(amb)//2]}/{max(amb)} ms")
        if res:
            print(f"[soak] resolve  min/median/max: {min(res)}/{sorted(res)[len(res)//2]}/{max(res)} ms")
    print("[soak] json: " + json.dumps(rows))
    return 1 if broken else 0


if __name__ == "__main__":
    sys.exit(main())
