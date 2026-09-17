#!/usr/bin/env python3
"""End-to-end test of the plan loop (PROTOCOL.md §6, §7; docs/UX.md §3.2).

The plan path is the only one where the *server* applies the edit: the client asks for a
step, the server resolves it against the content it holds, and then sends
`workspace/applyEdit` back to the client. So this test also exercises a direction the other
harnesses never touch.

    python3 verify/plan_test.py [--bin target/release/meta-lsp]

Exits 0 only when every check passes. Needs `python3` and no GPU.
"""
import argparse
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
sys.path.insert(0, HERE)

from smoke import Lsp, check, RESULTS  # noqa: E402

FIXTURE = """def load(path):
    f = open(path)
    return f
"""


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def start_stub(port):
    env = dict(os.environ, STUB_PORT=str(port))
    proc = subprocess.Popen(
        [sys.executable, os.path.join(HERE, "stub_model.py")],
        env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    for _ in range(100):
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{port}/health", timeout=0.5):
                return proc
        except Exception:
            time.sleep(0.05)
    raise RuntimeError("stub never became ready")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default=os.path.join(REPO, "target", "release", "meta-lsp"))
    args = ap.parse_args()
    RESULTS.clear()

    port = free_port()
    stub = start_stub(port)
    workdir = tempfile.mkdtemp(prefix="meta-plan-")
    fixture = os.path.join(workdir, "loader.py")
    with open(fixture, "w") as fh:
        fh.write(FIXTURE)
    uri = "file://" + fixture

    env = dict(os.environ, META_BASE_URL=f"http://127.0.0.1:{port}/v1",
               META_MODEL="stub-model", META_REVIEW_MODEL="stub-model")
    server = Lsp([args.bin, "--stdio"], env)
    try:
        server.request("initialize", {
            "processId": os.getpid(), "rootUri": "file://" + workdir,
            "capabilities": {"workspace": {"configuration": True}},
        })
        server.notify("initialized", {})
        server.notify("textDocument/didOpen", {"textDocument": {
            "uri": uri, "languageId": "python", "version": 1, "text": FIXTURE}})

        print("[plan] meta.plan")
        plan = server.request("workspace/executeCommand", {
            "command": "meta.plan",
            "arguments": [{"goal": "make the load path fail loudly",
                           "scope": {"uri": uri, "line": 1}}],
        }, timeout=60).get("result", {})

        check(plan.get("schema") == "meta.artifact/1", "the reply is an artifact")
        check(plan.get("kind") == "plan", "of kind plan (PROTOCOL §7)")
        check(isinstance(plan.get("id"), str) and plan["id"], "it carries an id")
        check(plan.get("goal") == "make the load path fail loudly", "and the goal")
        check(isinstance(plan.get("created"), str) and plan["created"].endswith("Z"),
              f"and a timestamp: {plan.get('created')}")
        steps = plan.get("steps") or []
        check(len(steps) >= 1, f"with at least one step ({len(steps)})")
        if not steps:
            return 1
        s0 = steps[0]
        check(s0.get("n") == 1 and isinstance(s0.get("title"), str), "steps are numbered and titled")
        check(s0.get("verb") in ("fix", "harden", "types", "docs", "rewrite", "test", "generate"),
              f"the verb is one of the served set: {s0.get('verb')}")
        check(s0.get("status") == "proposed", "a step starts proposed")
        targets = s0.get("targets") or []
        check(len(targets) == 1, "with a target")
        if targets:
            t = targets[0]
            check(t.get("uri") == uri, "naming the document")
            check(t.get("version") == 1, "stamped with the version it was computed against")
            check(isinstance(t.get("range", {}).get("start", {}).get("line"), int),
                  "and a real range")
        check("usage" in plan and plan["usage"].get("tokens_in", 0) > 0,
              "and the cost of producing it")

        print("[plan] meta.apply — the server applies through the client")
        before = open(fixture).read()
        applied = server.request("workspace/executeCommand", {
            "command": "meta.apply",
            "arguments": [{"plan_id": plan["id"], "steps": [1]}],
        }, timeout=60).get("result", {})
        check(applied.get("ok") is True, f"the step applied: {applied.get('failed')}")
        entries = applied.get("applied") or []
        check(len(entries) == 1, "and is reported back")
        edit_id = entries[0].get("edit_id") if entries else None
        check(isinstance(edit_id, str), f"with an edit id to revert: {edit_id}")
        after = open(fixture).read()
        check(after != before, "the file on disk changed")
        check("meta" in after, f"and carries the model's change: {after!r}")

        print("[plan] the plan records the step as applied")
        replanned = server.request("workspace/executeCommand", {
            "command": "meta.status", "arguments": []}).get("result", {})
        check(replanned.get("plans", 0) >= 1, "the plan is held for the session")

        print("[plan] meta.revert")
        reverted = server.request("workspace/executeCommand", {
            "command": "meta.revert", "arguments": [{"edit_id": edit_id}]},
            timeout=60).get("result", {})
        check(reverted.get("ok") is True, f"the revert succeeded: {reverted}")
        check(open(fixture).read() == before, "and the file is byte-for-byte what it was")

        print("[plan] reverting twice is refused, not a crash")
        again = server.request("workspace/executeCommand", {
            "command": "meta.revert", "arguments": [{"edit_id": edit_id}]},
            timeout=60).get("result", {})
        check(again.get("ok") is False and again.get("error", {}).get("code") == "unknown_edit",
              f"the second revert is refused: {again.get('error')}")

        print("[plan] a step whose target has moved")
        server.notify("textDocument/didChange", {
            "textDocument": {"uri": uri, "version": 2},
            "contentChanges": [{"text": "def load(path):\n    return None\n"}],
        })
        stale = server.request("workspace/executeCommand", {
            "command": "meta.apply",
            "arguments": [{"plan_id": plan["id"], "steps": [1]}],
        }, timeout=60).get("result", {})
        failed = (stale.get("failed") or [{}])[0]
        check(failed.get("code") == "stale",
              f"the step is refused as stale, not applied blindly: {failed}")

        print("[plan] a divergence between prediction and reality is published")
        # Its own document, so this cannot disturb the reverts above.
        fixture2 = os.path.join(workdir, "second.py")
        with open(fixture2, "w") as fh:
            fh.write(FIXTURE)
        uri2 = "file://" + fixture2
        server.notify("textDocument/didOpen", {"textDocument": {
            "uri": uri2, "languageId": "python", "version": 1, "text": FIXTURE}})
        plan2 = server.request("workspace/executeCommand", {
            "command": "meta.plan",
            "arguments": [{"goal": "same goal", "scope": {"uri": uri2, "line": 1}}],
        }, timeout=60).get("result", {})
        applied2 = server.request("workspace/executeCommand", {
            "command": "meta.apply",
            "arguments": [{"plan_id": plan2.get("id"), "steps": [1]}]},
            timeout=60).get("result", {})
        check(applied2.get("ok") is True, "the second plan applied")

        # The client now reports content that is NOT what the applied edit should have
        # produced. PROTOCOL §8 requires the server to notice and say so.
        server.notify("textDocument/didChange", {
            "textDocument": {"uri": uri2, "version": 2},
            "contentChanges": [{"text": "def load(path):\n    return None\n# something else entirely\n"}],
        })
        deadline = time.time() + 10
        diverged = []
        while time.time() < deadline and not diverged:
            for n in server.saw_notification("textDocument/publishDiagnostics"):
                if n["params"].get("uri") != uri2:
                    continue
                hits = [d for d in n["params"].get("diagnostics", [])
                        if d.get("code") == "meta.divergence"]
                if hits:
                    diverged = hits
            time.sleep(0.05)
        check(len(diverged) >= 1, "the divergence is published as a diagnostic")
        if diverged:
            d = diverged[0]
            check(d.get("severity") == 1, "at ERROR severity (PROTOCOL §9 reserves it for this)")
            check(d.get("source") == "meta", "from meta")
            check("predicted" in d.get("message", ""),
                  f"naming what was expected and what arrived: {d.get('message')}")

        print("[plan] a multi-file edit creates a file (U6)")
        # Script the endpoint to answer with a new file, and drive it through the ordinary
        # code-action path: the client applies that edit, exactly as Neovim would.
        request_body = json.dumps({
            "edit": {
                "summary": "add a test",
                "rationale": "Scripted: the missing-file path is untested.",
                "replacements": [],
                "new_files": [{"path": "test_loader.py",
                               "content": "def test_load():\n    assert True\n"}],
            }
        }).encode()
        req = urllib.request.Request(
            f"http://127.0.0.1:{port}/__script", data=request_body,
            headers={"content-type": "application/json"}, method="POST")
        urllib.request.urlopen(req, timeout=5).read()

        fixture3 = os.path.join(workdir, "third.py")
        with open(fixture3, "w") as fh:
            fh.write(FIXTURE)
        uri3 = "file://" + fixture3
        server.notify("textDocument/didOpen", {"textDocument": {
            "uri": uri3, "languageId": "python", "version": 1, "text": FIXTURE}})

        actions = server.request("textDocument/codeAction", {
            "textDocument": {"uri": uri3},
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}},
            "context": {"triggerKind": 1, "diagnostics": []},
        }, timeout=30).get("result") or []
        test_action = next((a for a in actions
                            if a.get("data", {}).get("verb") == "test"), None)
        if check(test_action is not None, "the test verb is offered in the menu"):
            resolved = server.request("codeAction/resolve", test_action, timeout=60).get("result", {})
            edit = resolved.get("edit") or {}
            changes = edit.get("documentChanges") or []
            kinds = [c.get("kind") for c in changes if "kind" in c]
            check("create" in kinds, f"the edit creates a file: {kinds}")
            created = os.path.join(workdir, "test_loader.py")
            check(not os.path.exists(created), "and it does not exist yet")
            ok, why = server.apply_edit(edit)
            check(ok, f"the client applies it: {why}")
            check(os.path.exists(created), "the new file exists afterwards")
            if os.path.exists(created):
                check(open(created).read().startswith("def test_load"),
                      "with the model's content")

        return 0 if all(ok for ok, _ in RESULTS) else 1
    finally:
        server.stop()
        stub.terminate()
        try:
            stub.wait(timeout=3)
        except subprocess.TimeoutExpired:
            stub.kill()
        shutil.rmtree(workdir, ignore_errors=True)
        passed = sum(1 for ok, _ in RESULTS if ok)
        print(f"\n[plan] {passed}/{len(RESULTS)} checks passed")


if __name__ == "__main__":
    sys.exit(main())
