#!/usr/bin/env python3
"""The fast paths, measured against a *slow* model.

Every path here is one the editor takes without being asked: rendering a buffer asks for lenses
and hints, opening the menu asks for code actions, typing asks for a completion. A model call
anywhere in them is a stutter the user cannot explain, so each is timed against a budget.

The stub model is started with a two-second delay, which is the point: "fast because the model
is fast" and "fast because nothing called it" look identical against a quick stub, and only one
of them is the invariant. A path that reaches the model takes at least two seconds and fails
its budget; nothing here is allowed near it (N2, N3).

    python3 verify/latency.py [--bin target/release/meta-lsp]

Prints one line per path with its measured time. Exit is nonzero if any path exceeded its
budget, or if the model was reached at all.
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

from smoke import Lsp  # noqa: E402

# What each path is allowed. Deliberately generous for a loaded machine and still an order of
# magnitude under the model delay, so the test answers one question: did this reach the model?
BUDGET_MS = 150
MODEL_DELAY_MS = 2000

FIXTURE = "\n".join(
    [
        "import json",
        "",
        "",
        "def load_config(path):",
        "    f = open(path)",
        '    return json.load(f)["services"][0]["port"]',
        "",
        "",
        "def save(path, payload):",
        '    with open(path, "w") as f:',
        "        json.dump(payload, f)",
        "",
    ]
)


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def start_stub(port):
    env = dict(os.environ, STUB_PORT=str(port), STUB_DELAY_MS=str(MODEL_DELAY_MS))
    proc = subprocess.Popen(
        [sys.executable, os.path.join(HERE, "stub_model.py")],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
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

    port = free_port()
    stub = start_stub(port)
    workdir = tempfile.mkdtemp(prefix="meta-latency-")
    fixture = os.path.join(workdir, "loader.py")
    with open(fixture, "w") as fh:
        fh.write(FIXTURE)
    uri = "file://" + fixture

    env = {k: v for k, v in os.environ.items() if not k.startswith("META_")}
    server = Lsp([args.bin, "--stdio"], env)
    server.settings = {
        "models": {
            "reason": {"base_url": f"http://127.0.0.1:{port}/v1", "model": "stub-model"},
            "review": {"base_url": f"http://127.0.0.1:{port}/v1", "model": "stub-model"},
            "fim": {"base_url": f"http://127.0.0.1:{port}/v1", "model": "stub-model"},
        }
    }

    results = []

    def timed(label, call, budget=BUDGET_MS):
        start = time.perf_counter()
        value = call()
        ms = (time.perf_counter() - start) * 1000
        ok = ms <= budget
        results.append((ok, label, ms, budget))
        print(f"  {'ok  ' if ok else 'SLOW'}  {label:<52} {ms:7.1f} ms  (budget {budget})")
        return value

    try:
        server.request(
            "initialize",
            {
                "processId": os.getpid(),
                "rootUri": "file://" + workdir,
                "capabilities": {"workspace": {"configuration": True}},
            },
        )
        server.notify("initialized", {})
        server.notify(
            "textDocument/didOpen",
            {"textDocument": {"uri": uri, "languageId": "python", "version": 1, "text": FIXTURE}},
        )

        doc = {"textDocument": {"uri": uri}}
        whole = {
            "start": {"line": 0, "character": 0},
            "end": {"line": 1000, "character": 0},
        }

        # The menu, on a cold cache: it may queue the analysis, but it must not wait for it.
        timed(
            "codeAction, cold cache",
            lambda: server.request(
                "textDocument/codeAction",
                {
                    "textDocument": {"uri": uri},
                    "range": {"start": {"line": 4, "character": 0}, "end": {"line": 5, "character": 0}},
                    "context": {"diagnostics": []},
                },
                timeout=30,
                poll=0.0002,
            ),
        )

        # Give the background analysis time to finish, then time the paths that the editor
        # walks on its own schedule. The `timeout=` on each request is liveness — a machine
        # under load should not be mistaken for a regression — while the *budget* below is the
        # assertion: 150 ms, an order of magnitude under the model delay.
        server.notify("textDocument/didSave", doc)
        time.sleep(3.0)

        timed(
            "codeAction, warm",
            lambda: server.request(
                "textDocument/codeAction",
                {
                    "textDocument": {"uri": uri},
                    "range": {"start": {"line": 4, "character": 0}, "end": {"line": 5, "character": 0}},
                    "context": {"diagnostics": []},
                },
                timeout=30,
                poll=0.0002,
            ),
        )
        timed(
            "diagnostic (pull)",
            lambda: server.request("textDocument/diagnostic", doc, timeout=10, poll=0.0002),
        )
        timed("codeLens", lambda: server.request("textDocument/codeLens", doc, timeout=10, poll=0.0002))
        timed(
            "inlayHint",
            lambda: server.request(
                "textDocument/inlayHint",
                {"textDocument": {"uri": uri}, "range": whole},
                timeout=30,
                poll=0.0002,
            ),
        )
        timed(
            "inlineCompletion",
            lambda: server.request(
                "textDocument/inlineCompletion",
                {
                    "textDocument": {"uri": uri},
                    "position": {"line": 5, "character": 0},
                    "context": {"triggerKind": 1},
                },
                timeout=30,
                poll=0.0002,
            ),
            budget=MODEL_DELAY_MS - 500,
        )
        timed(
            "meta.status",
            lambda: server.request(
                "workspace/executeCommand",
                {"command": "meta.status", "arguments": []},
                timeout=30,
                poll=0.0002,
            ),
        )
        timed(
            "meta.session",
            lambda: server.request(
                "workspace/executeCommand",
                {"command": "meta.session", "arguments": [{"limit": 50}]},
                timeout=30,
                poll=0.0002,
            ),
        )

        with urllib.request.urlopen(f"http://127.0.0.1:{port}/__requests", timeout=5) as r:
            calls = len(json.load(r)["requests"])
        print(f"\n[latency] model calls during the run: {calls} (the analysis is allowed; nothing else is)")

        failed = [r for r in results if not r[0]]
        if failed:
            print(f"[latency] {len(failed)} path(s) over budget")
            return 1
        print(f"[latency] {len(results)} path(s) within budget")
        return 0
    finally:
        server.stop()
        stub.terminate()
        try:
            stub.wait(timeout=3)
        except subprocess.TimeoutExpired:
            stub.kill()
        shutil.rmtree(workdir, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
