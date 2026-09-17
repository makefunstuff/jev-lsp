#!/usr/bin/env python3
"""CLI/LSP parity (U8): the same request through both front ends must agree exactly.

Both front ends are thin shells over `meta-core`, so for the same file, the same model, and
the same verb the conclusions must be identical — not merely similar. This drives the same
operation twice and compares the artefacts field by field.

    python3 verify/cli_parity.py [--bin target/release/meta-lsp] [--cli target/release/meta]

Exits 0 only when every comparison matches.
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
    ap.add_argument("--cli", default=os.path.join(REPO, "target", "release", "meta"))
    args = ap.parse_args()
    RESULTS.clear()

    port = free_port()
    stub = start_stub(port)
    workdir = tempfile.mkdtemp(prefix="meta-parity-")
    fixture = os.path.join(workdir, "loader.py")
    with open(fixture, "w") as fh:
        fh.write(FIXTURE)
    uri = "file://" + fixture

    env = dict(os.environ, META_BASE_URL=f"http://127.0.0.1:{port}/v1",
               META_MODEL="stub-model", META_REVIEW_MODEL="stub-model")

    def cli(*argv):
        out = subprocess.run([args.cli, *argv], capture_output=True, text=True,
                             env=env, timeout=60)
        return out

    server = Lsp([args.bin, "--stdio"], env)
    try:
        # ---- review: findings -------------------------------------------------
        print("[parity] review")
        cli_out = cli("review", fixture)
        check(cli_out.returncode == 0, f"the CLI review succeeded: {cli_out.stderr.strip()[:120]}")
        check(len(cli_out.stdout.strip().splitlines()) == 1, "one line of JSON on stdout")
        cli_review = json.loads(cli_out.stdout)

        server.request("initialize", {
            "processId": os.getpid(), "rootUri": "file://" + workdir,
            "capabilities": {"workspace": {"configuration": True}},
        })
        server.notify("initialized", {})
        server.notify("textDocument/didOpen", {"textDocument": {
            "uri": uri, "languageId": "python", "version": 1, "text": FIXTURE}})
        server.notify("textDocument/didSave", {"textDocument": {"uri": uri}})
        deadline = time.time() + 20
        items = []
        refreshes = 0
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

        cli_findings = [
            (d["line"], d["start_col"], d["end_col"], d["label"], d["severity"])
            for d in cli_review.get("diagnostics", [])
        ]
        lsp_findings = [
            (d["range"]["start"]["line"], d["range"]["start"]["character"],
             d["range"]["end"]["character"], d["message"].split(" — ")[0],
             "warning" if d["severity"] == 2 else "information")
            for d in items
        ]
        check(len(cli_findings) == len(lsp_findings),
              f"the same number of findings through both paths "
              f"(cli={len(cli_findings)}, lsp={len(lsp_findings)})")
        check(cli_findings == lsp_findings,
              f"and the same findings, in the same places:\n    cli={cli_findings}\n    lsp={lsp_findings}")

        # ---- action: the proposed edit ----------------------------------------
        print("[parity] action --verb harden")
        cli_action = json.loads(cli("action", "--verb", "harden", f"{fixture}:1").stdout)
        cli_edits = cli_action["edit"]["documentChanges"][0]["edits"]

        actions = server.request("textDocument/codeAction", {
            "textDocument": {"uri": uri},
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}},
            "context": {"triggerKind": 1, "diagnostics": []},
        }, timeout=30).get("result") or []
        picked = next((a for a in actions if a.get("data", {}).get("verb") == "harden"), None)
        if check(picked is not None, "the same verb is offered by the server"):
            resolved = server.request("codeAction/resolve", picked, timeout=60).get("result", {})
            lsp_edits = (resolved.get("edit") or {}).get("documentChanges", [{}])[0].get("edits", [])
            check(cli_edits == lsp_edits,
                  f"the proposed edit is identical through both paths:\n"
                  f"    cli={json.dumps(cli_edits)}\n    lsp={json.dumps(lsp_edits)}")

            # The action payload itself should agree on everything but the document version,
            # which each front end records for itself.
            ca, la = cli_action["action"], picked["data"]
            for key in ("verb", "language", "state", "scope_source"):
                check(ca.get(key) == la.get(key),
                      f"the action agrees on {key}: cli={ca.get(key)!r} lsp={la.get(key)!r}")
            check(ca.get("scope", {}).get("start_line") == la.get("scope", {}).get("start_line")
                  and ca.get("scope", {}).get("kind") == la.get("scope", {}).get("kind"),
                  "and on the scope it resolved")

        # ---- the CLI never writes --------------------------------------------
        print("[parity] the CLI is a proposer, never an editor")
        before = open(fixture).read()
        cli("action", "--verb", "rewrite", f"{fixture}:1")
        cli("review", fixture)
        check(open(fixture).read() == before, "the file is byte-identical after both commands")

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
        print(f"\n[parity] {passed}/{len(RESULTS)} checks passed")


if __name__ == "__main__":
    sys.exit(main())
