#!/usr/bin/env python3
"""A fix may not reach outside the scope the user asked about.

    python3 verify/scope_containment_test.py [--bin target/release/meta-lsp]

Measured bug: the model returned a whole-file rewrite, the server turned it into ops spanning
the document, and applying a fix replaced the user's entire buffer instead of editing the
function they had selected. Nothing bounded the answer to the scope.

The test scripts exactly that answer — a `file`-kind replacement — and requires the server to
refuse it, naming the scope, rather than hand the client an edit that rewrites everything. The
negative control is the rest of the suite: with the ordinary scoped edit from the stub, the same
pick-resolve-apply path succeeds (verify/smoke.py), so this is not passing merely because
resolution is broken.
"""

import json
import os
import socket
import subprocess
import sys
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from smoke import Lsp  # noqa: E402

OUT_OF_SCOPE = {
    "summary": "edit a different function",
    "rationale": "Scripted: an answer that reaches outside the scope it was asked about.",
    "replacements": [
        {
            # A statement in a *different* function: the answer is anchored, valid, and
            # outside the scope the user selected, which is the case that used to be applied.
            "anchor": {"kind": "statement", "match": "return 1"},
            "replacement": "return 42",
        }
    ],
    "new_files": [],
}


def main():
    bin_path = os.path.abspath(sys.argv[sys.argv.index("--bin") + 1]
                               if "--bin" in sys.argv else "target/release/meta-lsp")
    if not os.path.exists(bin_path):
        print(f"scope_containment_test: no binary at {bin_path}", file=sys.stderr)
        return 2

    port = 8113
    stub = subprocess.Popen([sys.executable, os.path.join(HERE, "stub_model.py")],
                            env=dict(os.environ, STUB_PORT=str(port)),
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(80):
            try:
                socket.create_connection(("127.0.0.1", port), timeout=1).close()
                break
            except OSError:
                time.sleep(0.1)
        else:
            print("scope_containment_test: the stub never came up", file=sys.stderr)
            return 2

        # Every model answer from here is a whole-file rewrite.
        req = urllib.request.Request(
            f"http://127.0.0.1:{port}/__script",
            data=json.dumps({"edit": OUT_OF_SCOPE}).encode(),
            headers={"content-type": "application/json"}, method="POST")
        urllib.request.urlopen(req, timeout=5).read()

        workdir = "/tmp/scope-containment"
        os.makedirs(workdir, exist_ok=True)
        path = os.path.join(workdir, "scoped.py")
        text = ("def first():\n"
                "    return 1\n"
                "\n"
                "\n"
                "def second(values):\n"
                "    total = 0\n"
                "    for value in values:\n"
                "        total += value\n"
                "    return total\n")
        with open(path, "w") as fh:
            fh.write(text)
        uri = "file://" + path

        server = Lsp([bin_path, "--stdio"],
                     {k: v for k, v in os.environ.items() if not k.startswith("META_")})
        server.settings = {
            "budget": {"max_calls_per_min": 30, "max_calls_per_hour": 100},
            "models": {"reason": {"base_url": f"http://127.0.0.1:{port}/v1", "model": "stub-model"},
                       "review": {"base_url": f"http://127.0.0.1:{port}/v1", "model": "stub-model"},
                       "fim": {"base_url": f"http://127.0.0.1:{port}/v1", "model": "stub-model"}},
        }
        server.request("initialize", {"processId": os.getpid(), "rootUri": "file:///tmp",
                                      "capabilities": {"workspace": {"configuration": True}}})
        server.notify("initialized", {})
        server.notify("textDocument/didOpen", {"textDocument": {
            "uri": uri, "languageId": "python", "version": 1, "text": text}})
        # The scope is `second`, so a file-wide answer is far outside it.
        server.notify("textDocument/didChange", {
            "textDocument": {"uri": uri, "version": 2},
            "contentChanges": [{"text": text}]})

        actions = server.request("textDocument/codeAction", {
            "textDocument": {"uri": uri},
            "range": {"start": {"line": 7, "character": 8}, "end": {"line": 7, "character": 8}},
            "context": {"diagnostics": []},
        }, timeout=60)
        listed = actions.get("result") or []
        if not listed:
            print("FAIL no code actions were offered, so nothing could be resolved")
            return 1

        resolved = server.request("codeAction/resolve", listed[0], timeout=120)
        server.stop()

        blob = json.dumps(resolved)
        message = ""
        payload = resolved.get("result") or {}
        disabled = payload.get("disabled") if isinstance(payload, dict) else None
        if isinstance(disabled, dict):
            message = str(disabled.get("reason", ""))[:200]
        elif isinstance(resolved.get("error"), dict):
            message = str(resolved["error"].get("message", ""))[:200]
        if "outside the scope" in blob:
            print("ok   an out-of-scope answer was refused, naming the scope: " + message)
            return 0
        print("FAIL an out-of-scope answer was turned into an edit instead of being refused")
        print("     got: " + blob[:400])
        return 1
    finally:
        stub.terminate()


if __name__ == "__main__":
    sys.exit(main())
