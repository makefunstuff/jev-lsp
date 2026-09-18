#!/usr/bin/env python3
"""The first model call of a session must obey the client's settings, not the built-in defaults.

    python3 verify/settings_race_test.py [--bin target/release/meta-lsp]

`workspace/configuration` is a round trip: the server asks after `initialized` and uses the
answer when it arrives. A request that lands in that window used to be answered from the
built-in defaults — the wrong endpoint, the wrong budget, the wrong model — which is how a
session can begin by calling somewhere the user never configured, and answer as if it had.

The test is discriminating by construction: the configured endpoint is the stub, and the
default is not. The call is made immediately after `initialized`, with no pause: if the server
waits for the settings, the answer carries the stub's marker; if it does not, the answer comes
from wherever the defaults point, and cannot.
"""

import json
import os
import socket
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from smoke import Lsp  # noqa: E402

MARKER = "SETTINGS_RACE_OK"
RAW = json.dumps({"summary": "from the stub", "markdown": f"# race\n\n{MARKER}\n"})


def main():
    bin_path = os.path.abspath(sys.argv[sys.argv.index("--bin") + 1]
                               if "--bin" in sys.argv else "target/release/meta-lsp")
    if not os.path.exists(bin_path):
        print(f"settings_race_test: no binary at {bin_path}", file=sys.stderr)
        return 2

    port = 8111
    stub_env = dict(os.environ, STUB_RAW=RAW, STUB_PORT=str(port))
    stub = subprocess.Popen([sys.executable, os.path.join(HERE, "stub_model.py")],
                            env=stub_env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    url = f"http://127.0.0.1:{port}/v1"
    try:
        # Readiness is the socket, not a route: the stub serves chat completions and nothing
        # else, so probing /models says "not up" about a stub that is answering perfectly well.
        for _ in range(80):
            try:
                socket.create_connection(("127.0.0.1", port), timeout=1).close()
                break
            except OSError:
                time.sleep(0.1)
        else:
            print("settings_race_test: the stub never came up", file=sys.stderr)
            return 2

        workdir = "/tmp/settings-race"
        os.makedirs(workdir, exist_ok=True)
        path = os.path.join(workdir, "racy.py")
        with open(path, "w") as fh:
            fh.write("def add(a, b):\n    return a + b\n")
        uri = "file://" + path

        env = {k: v for k, v in os.environ.items() if not k.startswith("META_")}
        server = Lsp([bin_path, "--stdio"], env)
        server.settings = {
            "models": {
                "reason": {"base_url": url, "model": "stub-model"},
                "review": {"base_url": url, "model": "stub-model"},
                "fim": {"base_url": url, "model": "stub-model"},
            },
        }
        server.request("initialize", {
            "processId": os.getpid(),
            "rootUri": "file:///tmp",
            "capabilities": {"workspace": {"configuration": True}},
        })
        server.notify("initialized", {})
        server.notify("textDocument/didOpen", {"textDocument": {
            "uri": uri, "languageId": "python", "version": 1,
            "text": open(path).read(),
        }})

        # No pause. This is the request that used to run on the defaults.
        res = server.request("workspace/executeCommand", {
            "command": "meta.explain",
            "arguments": [{"uri": uri, "line": 1}],
        }, timeout=60)
        server.stop()
        result = res.get("result") or {}
        blob = json.dumps(result)
        if MARKER in blob:
            print(f"ok   the first call used the configured endpoint ({MARKER} present)")
            return 0
        print("FAIL the first call did not reach the configured endpoint")
        print("     got: " + blob[:400])
        return 1
    finally:
        stub.terminate()


if __name__ == "__main__":
    sys.exit(main())
