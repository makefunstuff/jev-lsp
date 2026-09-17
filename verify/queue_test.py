#!/usr/bin/env python3
"""Regression test: an edit that lands while an analysis is in flight must not be lost.

The bug this locks down (found by `verify/lsp_client.py`, not by my own tests): a code
action opened the ambient analysis for the current content; an edit and a save then arrived
*while that model call was still running*; the save's request was dropped silently by the
single in-flight flag. The newer content therefore never got a conclusion, and no
`workspace/diagnostic/refresh` was ever sent for it — so the editor sat empty forever.

The stub is deliberately stalled (`STUB_DELAY_MS`) so the race is deterministic instead of
depending on timing. Two properties are asserted, both user-visible:

  * a refresh arrives after the save, even though the run it interrupted was superseded;
  * the findings a client pulls afterwards describe the *current* content.

    python3 verify/queue_test.py [--bin target/release/meta-lsp]
"""
import argparse
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
sys.path.insert(0, HERE)

from smoke import Lsp, check, RESULTS  # reuse the framing client; RESULTS is reset below

STALL_MS = 1500
V1 = """def run(p):
    data = open(p)
    return data
"""
V2 = """def run(p):
    data = open_checked(p)
    return data
"""


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def start_stub(port):
    env = dict(os.environ, STUB_PORT=str(port), STUB_DELAY_MS=str(STALL_MS))
    proc = subprocess.Popen(
        [sys.executable, os.path.join(HERE, "stub_model.py")],
        env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    import urllib.request
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
    workdir = tempfile.mkdtemp(prefix="meta-queue-")
    fixture = os.path.join(workdir, "thing.py")
    with open(fixture, "w") as fh:
        fh.write(V1)
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
            "uri": uri, "languageId": "python", "version": 1, "text": V1}})

        print("[queue] step 1: a code action starts an ambient analysis (cold cache)")
        server.request("textDocument/codeAction", {
            "textDocument": {"uri": uri},
            "range": {"start": {"line": 1, "character": 4}, "end": {"line": 1, "character": 4}},
            "context": {"triggerKind": 1, "diagnostics": []},
        })
        time.sleep(STALL_MS / 3 / 1000.0)  # the model call is now definitely in flight
        in_flight = len(server.saw_request("workspace/diagnostic/refresh")) == 0
        check(in_flight, "the first pass has not finished yet (the race is genuinely open)")

        print("[queue] step 2: an edit and a save land mid-flight")
        server.notify("textDocument/didChange", {
            "textDocument": {"uri": uri, "version": 2},
            "contentChanges": [{"text": V2}],
        })
        server.notify("textDocument/didSave", {"textDocument": {"uri": uri}})
        with open(fixture, "w") as fh:
            fh.write(V2)

        # Re-pull on EVERY refresh, bounded. A superseded pass refreshes too, and that
        # request can arrive before the pass for the content we now hold — so "wait for one
        # refresh, pull once" loses the finding. This is what a real client must do, and
        # getting it wrong is the bug that made step 8 look vacuous in lsp_client.py.
        refreshes_seen = 0
        items = []
        deadline = time.time() + 25
        while time.time() < deadline:
            seen = len(server.saw_request("workspace/diagnostic/refresh"))
            if seen <= refreshes_seen:
                time.sleep(0.05)
                continue
            refreshes_seen = seen
            report = server.request("textDocument/diagnostic",
                                    {"textDocument": {"uri": uri}}).get("result", {})
            items = report.get("items") or (
                report.get("fullDocumentDiagnosticReport", {}) or {}
            ).get("items", [])
            if items:
                break

        check(refreshes_seen >= 1,
              f"a refresh arrived after the save ({refreshes_seen} seen) — the interrupted "
              "content was not left without a conclusion")
        check(len(items) >= 1,
              f"a pull after a refresh carries the conclusion ({len(items)} finding(s), "
              f"after {refreshes_seen} refresh(es))")

        doc_lines = V2.split("\n")
        anchor = max((l for l in doc_lines if l.strip()), key=len)
        line = doc_lines.index(anchor)
        if items:
            check(items[0]["range"]["start"]["line"] == line,
                  f"and it describes the current content, line {line}")
            check(items[0]["data"]["finding_id"],
                  "the finding is dismissible (stable id present)")

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
        print(f"\n[queue] {passed}/{len(RESULTS)} checks passed")


if __name__ == "__main__":
    sys.exit(main())
