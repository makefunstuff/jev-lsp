#!/usr/bin/env python3
"""Inline completion (U7): the handler, and every gate in front of it.

`textDocument/inlineCompletion` is a 3.18-draft method, so this drives it by hand — Neovim
would only send it while the user types. What matters here is not the shape of the answer but
the gates: a completion fires on a timer, so anything that reaches the model on every
keystroke is the fastest way to burn a budget (PROTOCOL.md §5).

    python3 verify/inline_test.py [--bin target/release/meta-lsp]

Exits 0 only when every check passes.
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
    total = 0
    return total
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
    workdir = tempfile.mkdtemp(prefix="meta-inline-")
    fixture = os.path.join(workdir, "loader.py")
    with open(fixture, "w") as fh:
        fh.write(FIXTURE)
    uri = "file://" + fixture

    env = dict(os.environ, META_BASE_URL=f"http://127.0.0.1:{port}/v1",
               META_MODEL="stub-model", META_REVIEW_MODEL="stub-model")
    server = Lsp([args.bin, "--stdio"], env)

    def ask(line, character, trigger_kind=2, settings=None):
        """One inlineCompletion request; `settings` re-configures the server first."""
        if settings is not None:
            # The harness answers workspace/configuration from its own table, and the server
            # only re-reads when the client says the settings changed (PROTOCOL §10).
            server.settings = settings
            server.notify("workspace/didChangeConfiguration", {"settings": {"meta": settings}})
        res = server.request("textDocument/inlineCompletion", {
            "textDocument": {"uri": uri},
            "position": {"line": line, "character": character},
            "context": {"triggerKind": trigger_kind},
        }, timeout=60)
        items = ((res.get("result") or {}).get("items")) or []
        return items

    def model_calls():
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/__requests", timeout=5) as r:
            return len(json.load(r)["requests"])

    try:
        print("[inline] the capability is advertised")
        res = server.request("initialize", {
            "processId": os.getpid(), "rootUri": "file://" + workdir,
            "capabilities": {"workspace": {"configuration": True}},
        })
        caps = res.get("result", {}).get("capabilities", {})
        check("inlineCompletionProvider" in caps,
              "the server advertises inlineCompletionProvider (or Neovim never asks)")
        server.notify("initialized", {})
        server.notify("textDocument/didOpen", {"textDocument": {
            "uri": uri, "languageId": "python", "version": 1, "text": FIXTURE}})

        print("[inline] enabled: off by default")
        # `inline_completion.enabled` ships false (PROTOCOL §10), so nothing is served yet.
        before = model_calls()
        items = ask(1, 16)
        check(items == [], "no completion while inline completion is switched off")
        check(model_calls() == before, "and no model call is made either")
        # Every refusal must be silent: a popup while typing would be intolerable.
        check(server.saw_notification("window/showMessage") == [],
              "nothing is notified at the user while typing (docs/UX.md §6)")

        print("[inline] enabled: on")
        server.settings = {"inline_completion": {"enabled": True}}
        server.notify("workspace/didChangeConfiguration",
                      {"settings": {"meta": server.settings}})

        # No `meta.recompute` here: this is a fresh process with an empty cache, and a
        # recompute would start an ambient analysis whose model call lands asynchronously and
        # makes the count below racy.
        before = model_calls()
        items = ask(1, 16)
        check(len(items) == 1, f"one completion is offered when enabled ({len(items)})")
        if items:
            check(isinstance(items[0].get("insertText"), str) and items[0]["insertText"],
                  f"with text to insert: {items[0].get('insertText')!r}")
            check("range" not in items[0],
                  "and no range, so the client inserts at the cursor and nothing else")
        check(model_calls() == before + 1, "which costs exactly one model call")

        print("[inline] the gates in front of the model")
        before = model_calls()
        again = ask(1, 16)
        check(len(again) == 1, "the same cursor in the same content still answers")
        check(model_calls() == before, "but from cache: the model is not called twice")

        # The very start of the file: nothing before the cursor to complete from. A timed
        # request is refused there; an explicit one is answered anyway.
        before = model_calls()
        timed = ask(0, 0, trigger_kind=2)
        check(timed == [], "a timed request with no context is refused")
        check(model_calls() == before, "before any model call is made")

        invoked = ask(0, 0, trigger_kind=1)
        check(len(invoked) == 1,
              "but an explicit request is answered: the user asked, so it is not second-guessed")

        print("[inline] the limiter is its own window")
        server.settings = {"inline_completion": {"enabled": True, "max_calls_per_min": 1}}
        server.notify("workspace/didChangeConfiguration",
                      {"settings": {"meta": server.settings}})
        status = server.request("workspace/executeCommand",
                                {"command": "meta.status", "arguments": []},
                                timeout=30).get("result", {})
        check("fim_calls_last_minute" in status,
              f"status reports the completion traffic: {status.get('fim_calls_last_minute')}")

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
        print(f"\n[inline] {passed}/{len(RESULTS)} checks passed")


if __name__ == "__main__":
    sys.exit(main())
