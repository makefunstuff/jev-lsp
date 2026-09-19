#!/usr/bin/env python3
"""Regression test: a save during startup must not be analysed against the defaults.

Found by running the visual config by hand. A buffer saved before the first
`workspace/configuration` round trip finishes was analysed with the built-in model endpoint —
so a client that configured one watched the server call somewhere else and get nothing back.
Every other harness missed it because they all sleep before doing anything.

The reproduction is built in: the client stalls its configuration answer while the save is
already on the wire. Without the readiness gate the analysis runs immediately against
`http://127.0.0.1:8080/v1` (nothing is listening there), fails, and no finding ever appears.

    python3 verify/config_race_test.py [--bin target/release/jev-lsp]
"""
import argparse
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

FIXTURE = 'def load(path):\n    f = open(path)\n    return f\n'


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
    ap.add_argument("--bin", default=os.path.join(REPO, "target", "release", "jev-lsp"))
    args = ap.parse_args()
    RESULTS.clear()

    port = free_port()
    stub = start_stub(port)
    workdir = tempfile.mkdtemp(prefix="jev-race-")
    fixture = os.path.join(workdir, "loader.py")
    with open(fixture, "w") as fh:
        fh.write(FIXTURE)
    uri = "file://" + fixture

    # Deliberately no JEV_BASE_URL: the endpoint must come from the client's settings, and
    # the default points at a port nothing is listening on.
    env = {k: v for k, v in os.environ.items() if not k.startswith("JEV_")}
    server = Lsp([args.bin, "--stdio"], env)
    server.settings = {"rules": {"enabled": False}, "models": {
        "reason": {"base_url": f"http://127.0.0.1:{port}/v1", "model": "stub-model"},
        "review": {"base_url": f"http://127.0.0.1:{port}/v1", "model": "stub-model"},
    }}
    server.config_delay = 1.5  # the client is slow to say what it wants

    try:
        server.request("initialize", {
            "processId": os.getpid(), "rootUri": "file://" + workdir,
            "capabilities": {"workspace": {"configuration": True}},
        })
        server.notify("initialized", {})
        # The save goes out before the settings answer: exactly the startup race.
        server.notify("textDocument/didOpen", {"textDocument": {
            "uri": uri, "languageId": "python", "version": 1, "text": FIXTURE}})
        server.notify("textDocument/didSave", {"textDocument": {"uri": uri}})

        deadline = time.time() + 30
        items, refreshes = [], 0
        while time.time() < deadline:
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

        check(len(items) >= 1,
              f"the save was analysed with the endpoint the client configured ({len(items)} finding(s))")
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/__requests", timeout=5) as r:
            import json
            calls = len(json.load(r)["requests"])
        check(calls >= 1, f"and the configured endpoint is where the call went ({calls} call(s))")
        for m in server.saw_notification("window/logMessage"):
            if "settings applied" in m["params"]["message"]:
                check(str(port) in m["params"]["message"],
                      f"the server says which endpoint is in force: {m['params']['message']}")
                break
        else:
            check(False, "the server logs the endpoint it is using")

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
        print(f"\n[config-race] {passed}/{len(RESULTS)} checks passed")


if __name__ == "__main__":
    sys.exit(main())
