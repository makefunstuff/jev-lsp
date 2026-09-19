#!/usr/bin/env python3
"""CLI/LSP parity (U8): the same request through both front ends must agree exactly.

Both front ends are thin shells over `jev-core`, so for the same file, the same model, and
the same verb the conclusions must be identical — not merely similar. This drives the same
operation twice and compares the artefacts field by field.

    python3 verify/cli_parity.py [--bin target/release/jev-lsp] [--cli target/release/jev]

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
    ap.add_argument("--bin", default=os.path.join(REPO, "target", "release", "jev-lsp"))
    ap.add_argument("--cli", default=os.path.join(REPO, "target", "release", "jev"))
    args = ap.parse_args()
    RESULTS.clear()

    port = free_port()
    stub = start_stub(port)
    workdir = tempfile.mkdtemp(prefix="jev-parity-")
    # A repository, because `.jev/rules/` lives at the repository root: a flat fixture cannot
    # show whether the two front ends resolve the same root, and that is how a defect survived
    # where the CLI looked for the rules beside the file while the server used the workspace.
    subprocess.run(["git", "-C", workdir, "init", "-q"], capture_output=True)
    fixture = os.path.join(workdir, "loader.py")
    with open(fixture, "w") as fh:
        fh.write(FIXTURE)
    uri = "file://" + fixture

    env = dict(os.environ, JEV_BASE_URL=f"http://127.0.0.1:{port}/v1",
               JEV_MODEL="stub-model", JEV_REVIEW_MODEL="stub-model")

    def cli(*argv):
        out = subprocess.run([args.cli, *argv], capture_output=True, text=True,
                             env=env, timeout=60)
        return out

    server = Lsp([args.bin, "--stdio"], env)
    server.settings = {"rules": {"enabled": False}}
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

        # ---- inspect: the rules pass -----------------------------------------
        # The rules pass is the one path both front ends run through the same code, so it is the
        # one place where "field by field" is a statement about the shared implementation rather
        # than about two implementations that happen to agree.
        print("[parity] inspect")
        rules_dir = os.path.join(workdir, ".jev", "rules")
        os.makedirs(rules_dir)
        with open(os.path.join(rules_dir, "a.json"), "w") as fh:
            fh.write(json.dumps({"schema": "jev.rules/1", "rules": [{
                "id": "no-open",
                "title": "Unclosed file handle",
                "text": "A file opened here is never closed.",
                "severity": "warning",
                "applies_to": ["**/*.py"],
                "inspection": {"kind": "regex", "pattern": r"open\("},
                "judgement": {
                    "question": "Is this handle left open?",
                    "criteria": {"true": "nothing closes it", "false": "it is closed later"},
                    "min_probability": 0.75,
                },
                "verb_hint": "fix",
            }]}, indent=2))
        # The decision tier is a different protocol from the chat tiers, so it needs its own
        # endpoint variable — and the CLI flag, which the parity harness passes to both.
        inspect_env = dict(env, JEV_DECIDE_BASE_URL=f"http://127.0.0.1:{port}/v1")
        cli_out = subprocess.run([args.cli, "inspect", fixture], capture_output=True, text=True,
                                 env=inspect_env, timeout=60)
        check(cli_out.returncode == 0,
              f"the CLI inspect succeeded: {cli_out.stderr.strip()[:160]}")
        cli_inspect = json.loads(cli_out.stdout)

        lsp_server = Lsp([args.bin, "--stdio"], inspect_env)
        try:
            lsp_server.request("initialize", {
                "processId": os.getpid(), "rootUri": "file://" + workdir,
                "capabilities": {"workspace": {"configuration": True}},
            })
            lsp_server.notify("initialized", {})
            lsp_server.notify("textDocument/didOpen", {"textDocument": {
                "uri": uri, "languageId": "python", "version": 1, "text": before}})
            lsp_result = (lsp_server.request("workspace/executeCommand", {
                "command": "jev.inspect",
                "arguments": [{"path": fixture, "force": True}],
            }, timeout=60).get("result") or {})
        finally:
            lsp_server.stop()

        check(cli_inspect.get("schema") == lsp_result.get("schema") == "jev.result/1",
              f"both front ends answer with the same schema "
              f"(cli={cli_inspect.get('schema')}, lsp={lsp_result.get('schema')})")
        check(cli_inspect.get("ok") is True and lsp_result.get("ok") is True,
              "and both succeeded")
        for key in ("considered", "candidates"):
            check(cli_inspect.get(key) == lsp_result.get(key),
                  f"the same {key}: cli={cli_inspect.get(key)} lsp={lsp_result.get(key)}")
        cli_findings = [(f["line"], f["start_col"], f["end_col"], f["label"], f["severity"],
                         f["verb"], f["detail"]) for f in cli_inspect.get("findings") or []]
        lsp_findings = [(f["line"], f["start_col"], f["end_col"], f["label"], f["severity"],
                         f["verb"], f["detail"]) for f in lsp_result.get("findings") or []]
        check(cli_findings == lsp_findings and len(cli_findings) >= 1,
              f"and the same findings, field for field:\n"
              f"    cli={cli_findings}\n    lsp={lsp_findings}")
        check(cli_inspect.get("skipped") == lsp_result.get("skipped"),
              f"including what was skipped: cli={cli_inspect.get('skipped')} "
              f"lsp={lsp_result.get('skipped')}")
        check(open(fixture).read() == before,
              "and neither front end wrote to the file")

        # ---- inspect, with nothing to run ------------------------------------
        # The case a matching fixture cannot catch, and which shipped as a defect: a file no rule
        # claims must be reported as *skipped* by both front ends, with the same words. Asserting
        # only that both are non-empty would have passed while the CLI reported nothing at all.
        print("[parity] inspect, nothing to run")
        notes = os.path.join(workdir, "notes.md")
        with open(notes, "w") as fh:
            fh.write("# notes\n")
        notes_uri = "file://" + notes

        cli_out = subprocess.run([args.cli, "inspect", notes], capture_output=True, text=True,
                                 env=inspect_env, timeout=60)
        check(cli_out.returncode == 0,
              f"the CLI inspect of an unclaimed file succeeded: {cli_out.stderr.strip()[:160]}")
        cli_nothing = json.loads(cli_out.stdout)

        lsp_server = Lsp([args.bin, "--stdio"], inspect_env)
        try:
            lsp_server.request("initialize", {
                "processId": os.getpid(), "rootUri": "file://" + workdir,
                "capabilities": {"workspace": {"configuration": True}},
            })
            lsp_server.notify("initialized", {})
            lsp_server.notify("textDocument/didOpen", {"textDocument": {
                "uri": notes_uri, "languageId": "markdown", "version": 1, "text": "# notes\n"}})
            lsp_nothing = (lsp_server.request("workspace/executeCommand", {
                "command": "jev.inspect",
                "arguments": [{"path": notes, "force": True}],
            }, timeout=60).get("result") or {})
        finally:
            lsp_server.stop()

        check(cli_nothing.get("considered") == lsp_nothing.get("considered") == 0
              and cli_nothing.get("candidates") == lsp_nothing.get("candidates") == 0,
              f"neither pass found anything to run "
              f"(cli={cli_nothing.get('considered')}/{cli_nothing.get('candidates')}, "
              f"lsp={lsp_nothing.get('considered')}/{lsp_nothing.get('candidates')})")
        cli_skips = cli_nothing.get("skipped") or []
        lsp_skips = lsp_nothing.get("skipped") or []
        check(cli_skips == lsp_skips and len(cli_skips) == 1,
              f"and both front ends report the same skip, word for word:\n"
              f"    cli={cli_skips}\n    lsp={lsp_skips}")
        check(bool(cli_skips) and cli_skips[0].get("code") == "no_rules"
              and "nothing to run" in (cli_skips[0].get("detail") or ""),
              f"which names the reason rather than reporting a clean file: {cli_skips}")
        check(not (cli_nothing.get("findings") or lsp_nothing.get("findings")),
              "with no findings invented on either side")

        # ---- inspect, a nested file and a repository-relative pattern ---------
        # Two defects in one case: the CLI used to look for `.jev/rules/` beside the file rather
        # than at the repository root, and `applies_to` used to be matched against the absolute
        # path, so the pattern every rule author writes first matched nothing. Both were silent —
        # `no_rules` reads like "you have no rules" — and both are only visible with a nested
        # fixture in a repository.
        print("[parity] inspect, a nested file and a relative applies_to")
        nested_dir = os.path.join(workdir, "nested", "deep")
        os.makedirs(nested_dir)
        nested = os.path.join(nested_dir, "mod.py")
        with open(nested, "w") as fh:
            fh.write("def load(path):\n    return open(path)\n")
        with open(os.path.join(rules_dir, "a.json"), "w") as fh:
            fh.write(json.dumps({"schema": "jev.rules/1", "rules": [{
                "id": "no-open",
                "title": "Unclosed file handle",
                "text": "A file opened here is never closed.",
                "severity": "warning",
                "applies_to": ["nested/**/*.py"],
                "inspection": {"kind": "regex", "pattern": r"open\("},
                "judgement": {"question": "Is this handle left open?",
                              "criteria": {"true": "nothing closes it", "false": "it is closed"},
                              "min_probability": 0.75},
                "verb_hint": "fix",
            }]}, indent=2))

        cli_out = subprocess.run([args.cli, "inspect", "--force", nested], capture_output=True,
                                 text=True, env=inspect_env, timeout=60)
        check(cli_out.returncode == 0,
              f"the CLI inspect of a nested file succeeded: {cli_out.stderr.strip()[:160]}")
        cli_nested = json.loads(cli_out.stdout)

        lsp_server = Lsp([args.bin, "--stdio"], inspect_env)
        try:
            lsp_server.request("initialize", {
                "processId": os.getpid(), "rootUri": "file://" + workdir,
                "capabilities": {"workspace": {"configuration": True}},
            })
            lsp_server.notify("initialized", {})
            lsp_server.notify("textDocument/didOpen", {"textDocument": {
                "uri": "file://" + nested, "languageId": "python", "version": 1,
                "text": "def load(path):\n    return open(path)\n"}})
            lsp_nested = (lsp_server.request("workspace/executeCommand", {
                "command": "jev.inspect",
                "arguments": [{"path": nested, "force": True}],
            }, timeout=60).get("result") or {})
        finally:
            lsp_server.stop()

        check(cli_nested.get("considered") == lsp_nested.get("considered") == 1,
              f"a repository-relative pattern matches a nested file through both front ends "
              f"(cli={cli_nested.get('considered')}, lsp={lsp_nested.get('considered')})")
        check(cli_nested.get("candidates") == lsp_nested.get("candidates") == 1,
              f"and both found the same candidate "
              f"(cli={cli_nested.get('candidates')}, lsp={lsp_nested.get('candidates')})")
        check(cli_nested.get("skipped") == lsp_nested.get("skipped") == [],
              f"with neither reporting a skip: cli={cli_nested.get('skipped')} "
              f"lsp={lsp_nested.get('skipped')}")
        check(len(cli_nested.get("findings") or []) == len(lsp_nested.get("findings") or []) == 1,
              "and the same finding, from the same root")

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
