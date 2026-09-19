#!/usr/bin/env python3
"""Real-model experiment: run the live server against a real endpoint and report what happens.

This is deliberately *not* a pass/fail suite like `smoke.py`. That one asserts stub-shaped
expectations (exactly one model call, the finding lands on the scripted anchor), which a real
model has no reason to satisfy. Here the loop is the subject: does the pipeline complete, how
long does it take, what did the model actually produce, and does the edit apply cleanly.

    # local llama.cpp
    JEV_BASE_URL=http://127.0.0.1:8080/v1 JEV_MODEL=<model> python3 verify/real_model.py

    # the omp auth gateway (resolves the provider credential server-side; no key handling)
    python3 verify/real_model.py --base-url http://127.0.0.1:4000/v1 --model deepseek/deepseek-flash

    # a hosted endpoint that wants a key: JEV_API_KEY_ENV names the variable holding it
    JEV_API_KEY_ENV=OPENROUTER_API_KEY python3 verify/real_model.py \
        --base-url https://openrouter.ai/api/v1 --model google/gemini-2.5-flash-lite

The ambient pass measured here is the *chat review*: `rules.enabled` is off in this harness's
settings, because the ambient pass is the rules pass while rules are on and a workspace with no
`.jev/rules` gives it nothing to run. A run whose endpoint billed no tokens and logged a failure
exits non-zero with the server's words rather than reporting zeros as a result.

Exits 0 if the loop completed end to end; prints everything it observed either way.
"""
import argparse
import difflib
import os
import sys
import time
import shutil
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
sys.path.insert(0, HERE)

from smoke import Lsp  # framing client only

FIXTURE = '''import json


def load_first_name(path):
    f = open(path)
    data = json.load(f)
    return data["items"][0]["name"]


def total(prices):
    t = 0
    for p in prices:
        t = t + p
    return t
'''


def line_of(text, needle):
    for i, line in enumerate(text.split("\n")):
        if needle in line:
            return i
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default=os.path.join(REPO, "target", "release", "jev-lsp"))
    ap.add_argument("--base-url", default=os.environ.get("JEV_BASE_URL", "http://127.0.0.1:8080/v1"))
    ap.add_argument("--model", default=os.environ.get("JEV_MODEL", "qwen2.5-coder-7b-instruct"))
    ap.add_argument("--timeout", type=float, default=120.0, help="seconds to wait for a model call")
    ap.add_argument("--verb", default="auto", help="code action to resolve: auto, fix, harden, docs, test, explain")
    ap.add_argument("--keep", action="store_true")
    args = ap.parse_args()

    workdir = tempfile.mkdtemp(prefix="jev-real-")
    fixture = os.path.join(workdir, "loader.py")
    with open(fixture, "w") as fh:
        fh.write(FIXTURE)
    uri = "file://" + fixture

    env = dict(os.environ, JEV_BASE_URL=args.base_url, JEV_MODEL=args.model,
               JEV_REVIEW_MODEL=args.model)
    print(f"fixture : {fixture}")
    print(f"endpoint: {args.base_url}")
    print(f"model   : {args.model}")
    print(f"server  : {args.bin}\n")

    server = Lsp([args.bin, "--stdio"], env)
    # The ambient pass is the rules pass while `rules.enabled` is at its default, and this
    # workspace carries no `.jev/rules` — so with rules on, the save below runs a pass with
    # nothing to run and the review tier is never asked. This harness is about the model's
    # opinion, so the ambient slot is handed to the chat review explicitly (the same setting
    # `lsp_client.py` and `outcome_test.py` carry).
    server.settings = {"rules": {"enabled": False}}
    observed = {"loop": False}
    try:
        server.request("initialize", {
            "processId": os.getpid(), "rootUri": "file://" + workdir,
            "capabilities": {"workspace": {"configuration": True}},
        })
        server.notify("initialized", {})
        server.notify("textDocument/didOpen", {"textDocument": {
            "uri": uri, "languageId": "python", "version": 1, "text": FIXTURE}})

        # ---- ambient pass -------------------------------------------------
        print("== ambient review pass ==")
        t0 = time.time()
        server.notify("textDocument/didSave", {"textDocument": {"uri": uri}})
        items, refreshes = [], 0
        while time.time() - t0 < args.timeout:
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
            time.sleep(0.1)
        elapsed = time.time() - t0
        observed["loop"] = refreshes > 0
        print(f"  refreshes  : {refreshes}   wall: {elapsed:.1f}s")
        print(f"  findings   : {len(items)}")
        for d in items:
            ln = d["range"]["start"]["line"]
            print(f"    line {ln + 1}: severity={d['severity']} {d['message'][:110]}")
        for m in server.saw_notification("window/logMessage"):
            print(f"  server log : {m['params']['message'][:150]}")

        status = server.request("workspace/executeCommand",
                                {"command": "jev.status", "arguments": []}).get("result", {})
        b = status.get("budget", {})
        print(f"  budget     : calls_min={b.get('calls_last_minute')} "
              f"tokens={b.get('tokens_used')} in_flight={b.get('in_flight')}")

        # ---- resolve ------------------------------------------------------
        print("\n== code action ==")
        actions = server.request("textDocument/codeAction", {
            "textDocument": {"uri": uri},
            "range": {"start": {"line": 2, "character": 4}, "end": {"line": 2, "character": 4}},
            "context": {"triggerKind": 1, "diagnostics": []},
        }).get("result") or []
        print(f"  offered    : {len(actions)}")
        for a in actions:
            print(f"    {a['kind']:26} {a['title'][:70]}")

        wanted = args.verb
        if wanted == "auto":
            pick = next((a for a in actions if a["kind"] == "quickfix.jev"), None) \
                or next((a for a in actions if a["kind"] == "refactor.rewrite.jev"), None)
        elif wanted == "fix":
            pick = next((a for a in actions if a["kind"] == "quickfix.jev"), None)
        elif wanted == "explain":
            pick = None
        else:
            pick = next((a for a in actions
                         if a.get("data", {}).get("verb") == wanted), None)

        if pick is None:
            print("  (nothing to resolve for that verb)")
        else:
            print(f"  resolving  : {pick['title']}")
            t1 = time.time()
            resolved = server.request("codeAction/resolve", pick, timeout=args.timeout + 30).get("result", {})
            took = time.time() - t1
            edit = resolved.get("edit")
            state = resolved.get("data", {}).get("state")
            print(f"  wall       : {took:.1f}s   state={state}")
            if edit:
                before = FIXTURE.split("\n")
                after = list(before)
                for one in (edit.get("documentChanges") or []):
                    if "edits" not in one:
                        continue
                    for e in one["edits"]:
                        te = e.get("textEdit", e)
                        r = te["range"]
                        if r["start"]["line"] == r["end"]["line"]:
                            ln = r["start"]["line"]
                            after[ln] = (after[ln][:r["start"]["character"]]
                                         + te["newText"] + after[ln][r["end"]["character"]:])
                            continue
                        new = te["newText"].split("\n")
                        after[r["start"]["line"]:r["end"]["line"] + 1] = new
                print("\n  --- proposed diff ---")
                for line in difflib.unified_diff(before, after, "before", "after", lineterm="", n=2):
                    print("  " + line)
                with open(fixture, "w") as fh:
                    fh.write("\n".join(after))

                # Post-apply verification (PROTOCOL §8): an edit that leaves the file
                # unparseable is worse than no edit, and a model can produce one.
                import ast
                text = "\n".join(after)
                try:
                    ast.parse(text)
                    print("\n  syntax     : still parses")
                    observed["valid"] = True
                except SyntaxError as e:
                    print(f"\n  syntax     : BROKEN at line {e.lineno}: {e.msg}")
                    observed["valid"] = False
                dupes = [l for l in set(after) if l.strip() and after.count(l) > 1]
                if dupes:
                    print(f"  repeated   : {len(dupes)} line(s) now appear more than once: "
                          + "; ".join(d.strip()[:40] for d in dupes[:3]))
                print(f"  applied to {fixture}")
                observed["edit"] = True
            else:
                reason = (resolved.get("disabled") or {}).get("reason", "(none)")
                print(f"  no edit returned. reason: {reason}")
                observed["edit"] = False
                observed["resolve_reason"] = reason
            for m in server.saw_notification("window/showMessage"):
                print(f"  message    : {m['params']['message'][:200]}")

        status = server.request("workspace/executeCommand",
                                {"command": "jev.status", "arguments": []}).get("result", {})
        b = status.get("budget", {})
        print(f"\ntotals: calls_last_minute={b.get('calls_last_minute')} tokens={b.get('tokens_used')} "
              f"refusals={status.get('counters', {}).get('refusals')}")

        # A harness that reached no model must not report zeros as a result. A failed pass still
        # refreshes diagnostics, so `loop` above is true while nothing was asked of any model;
        # the endpoint billing no tokens plus a failure in the server's log is what says so.
        if not b.get("tokens_used"):
            failure = observed.get("resolve_reason") or next(
                (
                    m["params"].get("message", "")
                    for m in server.saw_notification("window/logMessage")
                    if "failed" in (m["params"].get("message") or "")
                ),
                "",
            )
            if failure:
                print(f"  unreachable: {failure}")
                return 1

        server.request("shutdown", None)
        server.notify("exit", None)
        return 0 if observed.get("loop") else 1
    finally:
        server.stop()
        if not args.keep:
            shutil.rmtree(workdir, ignore_errors=True)
        else:
            print(f"(kept {workdir})")


if __name__ == "__main__":
    sys.exit(main())
