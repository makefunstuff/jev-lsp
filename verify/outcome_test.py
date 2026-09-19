#!/usr/bin/env python3
"""Does the server ever learn what the user did with what it offered?

    python3 verify/outcome_test.py [--bin target/release/jev-lsp]

Measured gap: the client applied edits, dismissed findings and accepted completions locally,
and none of it reached the server — so "is this working" had no answer that was not a guess.
`jev.outcome` is the client's report and `jev.usage` is the answer, and this test is the one
place both ends are exercised against the real binary.

It discriminates: on a server without `jev.outcome`, the command answers `not_implemented` and
`jev.usage` does not exist, so `applied` cannot be read at all. Run it against the pre-change
binary once — that is how the test is known to be measuring the change rather than the stub.

No GPU and no network: the model is `verify/stub_model.py`, whose review answer is one finding
anchored at the longest line, which is all this needs to have something published.
"""

import argparse
import json
import os
import shutil
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
sys.path.insert(0, HERE)

from smoke import Lsp, Stub  # noqa: E402

FIXTURE = """def load(path):
    fh = open(path)
    return json.load(fh)
"""

FAILURES = []


def check(ok, what):
    print(("ok   " if ok else "FAIL ") + what)
    if not ok:
        FAILURES.append(what)
    return ok


def trace_for(root):
    return os.path.join(root, ".git", "jev", "session.jsonl")


def entries(path):
    """Every complete line of the record, newest last. A torn line is skipped, not fatal."""
    if not os.path.isfile(path):
        return []
    out = []
    with open(path) as fh:
        for line in fh:
            try:
                out.append(json.loads(line))
            except Exception:
                continue
    return out


def report(server, event):
    """One `jev.outcome` — what the client sends when the user does something. Returned as the
    raw answer so the checks can assert on it; every call here must succeed."""
    return server.request("workspace/executeCommand", {
        "command": "jev.outcome", "arguments": [event],
    })


def usage(server):
    """`jev.usage` as a Result envelope, or the error that came instead."""
    res = server.request("workspace/executeCommand", {"command": "jev.usage", "arguments": []})
    if isinstance(res.get("error"), dict):
        return None, res["error"]
    return res.get("result") or {}, None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default=os.path.join(REPO, "target", "release", "jev-lsp"))
    args = ap.parse_args()
    if not os.path.exists(args.bin):
        print(f"outcome_test: no binary at {args.bin}", file=sys.stderr)
        return 2

    stub = Stub()
    workdir = tempfile.mkdtemp(prefix="jev-outcome-")
    fixture = os.path.join(workdir, "outcome.py")
    with open(fixture, "w") as fh:
        fh.write(FIXTURE)
    uri = "file://" + fixture
    trace = trace_for(workdir)

    env = dict(
        os.environ,
        JEV_BASE_URL=f"http://127.0.0.1:{stub.port}/v1",
        JEV_MODEL="stub-model",
        JEV_REVIEW_MODEL="stub-model",
    )
    server = Lsp([args.bin, "--stdio"], env)
    server.settings = {"rules": {"enabled": False}}
    try:
        server.request("initialize", {
            "processId": os.getpid(),
            "rootUri": "file://" + workdir,
            "capabilities": {"workspace": {"configuration": True, "workspaceFolders": True}},
        })
        server.notify("initialized", {})
        server.notify("textDocument/didOpen", {"textDocument": {
            "uri": uri, "languageId": "python", "version": 1, "text": FIXTURE}})
        server.notify("textDocument/didSave", {"textDocument": {"uri": uri}})

        # One analysis, so there is something the report can be about.
        deadline = time.time() + 30
        published = None
        while time.time() < deadline:
            for e in entries(trace):
                if e.get("kind") == "analysis" and e.get("uri") == uri:
                    published = e
            if published is not None:
                break
            time.sleep(0.1)
        check(published is not None, "the analysis was recorded, so the report has a subject")
        if published is not None:
            listed = published.get("findings")
            check(isinstance(listed, list) and len(listed) == published.get("count"),
                  "the analysis entry lists the findings it kept as well as counting them")

        # What the client does when the user picks an edit: one report, no fanfare.
        applied = server.request("workspace/executeCommand", {
            "command": "jev.outcome",
            "arguments": [{"kind": "action-applied", "verb": "harden", "line": 3}],
        })
        check(applied.get("error") is None, "jev.outcome is served, not not_implemented")
        check((applied.get("result") or {}).get("recorded") is True,
              "and it answers that it recorded the event")

        result, err = usage(server)
        check(err is None, f"jev.usage is served (got {err})")
        result = result or {}
        check(result.get("applied") == 1, f"one applied edit is counted (got {result.get('applied')})")
        check(result.get("dismissed") == 0 and result.get("undone") == 0,
              "nothing else is counted as done")
        check(result.get("files") == 1, f"one file was analysed (got {result.get('files')})")
        check((result.get("published") or 0) >= 1, "the published finding reached the report")
        check(result.get("window") == "session log",
              "the report says what window its numbers are over")
        check(isinstance(result.get("markdown"), str) and result["markdown"],
              "the report is also an artifact, so `:Jev usage` can open it")

        # The log is a record, not a schema: an event this server has never heard of is written
        # as it arrived, and counted as nothing.
        report(server, {"kind": "not-a-real-event", "note": "kept verbatim"})
        result, _ = usage(server)
        check((result or {}).get("applied") == 1, "an unknown event changes no count")

        report(server, {"kind": "finding-dismissed", "id": "abc123", "line": 1})
        report(server, {"kind": "action-dismissed"})
        report(server, {"kind": "edit-undone"})
        result, _ = usage(server)
        result = result or {}
        check(result.get("dismissed") == 2,
              f"a dismissed finding and a dismissed action are both dismissals (got {result.get('dismissed')})")
        check(result.get("undone") == 1, "an undone edit is counted")
        check(result.get("applied") == 1, "and applying one edit is still one")

        outcomes = [e for e in entries(trace) if e.get("kind") == "outcome"]
        kinds = [e.get("event") for e in outcomes]
        check(kinds == ["action-applied", "not-a-real-event", "finding-dismissed",
                        "action-dismissed", "edit-undone"],
              f"every report landed in the record, in order (got {kinds})")
        first = next((e for e in outcomes if e.get("event") == "action-applied"), {})
        check(first.get("verb") == "harden" and first.get("line") == 3,
              "the report kept the verb and the line it was made about")
        unknown = next((e for e in outcomes if e.get("event") == "not-a-real-event"), {})
        check(unknown.get("note") == "kept verbatim",
              "an unknown event's fields are recorded verbatim")
    finally:
        server.stop()
        stub.stop()
        shutil.rmtree(workdir, ignore_errors=True)

    if FAILURES:
        print(f"\noutcome_test: {len(FAILURES)} check(s) failed")
        return 1
    print("\noutcome_test: every check passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
