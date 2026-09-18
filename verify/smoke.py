#!/usr/bin/env python3
"""End-to-end smoke test: the real `meta-lsp` binary against the scripted model.

This is the fast pre-flight. `verify/lsp_client.py` is the independent, spec-derived
conformance client; this one exists to answer "does the thing work at all" in one command.

    python3 verify/smoke.py [--bin target/release/meta-lsp]

Exits 0 only when every check passes. Never needs a GPU or the network.
"""
import argparse
import json
import os
import select
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)


def free_port():
    """Bind :0 and take what the kernel gives, so a supervised stub cannot collide."""
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


FIXTURE = """fn run(p: &str) -> Result<()> {
    let f = File::open(p)?;
    Ok(())
}
"""


class Stub:
    def __init__(self, port=None):
        self.port = port or free_port()
        env = dict(os.environ, STUB_PORT=str(self.port))
        self.proc = subprocess.Popen(
            [sys.executable, os.path.join(HERE, "stub_model.py")],
            env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        self.wait_ready()

    def url(self, path):
        return f"http://127.0.0.1:{self.port}{path}"

    def wait_ready(self):
        for _ in range(100):
            try:
                with urllib.request.urlopen(self.url("/health"), timeout=0.5):
                    return
            except Exception:
                time.sleep(0.05)
        raise RuntimeError("stub model never became ready")

    def requests(self):
        with urllib.request.urlopen(self.url("/__requests"), timeout=2) as r:
            return json.load(r)

    def stop(self):
        self.proc.terminate()
        try:
            self.proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.proc.kill()


class Lsp:
    """Minimal stdio LSP client with a reader thread."""

    def __init__(self, cmd, env):
        self.proc = subprocess.Popen(
            cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env
        )
        self.next_id = 0
        self.docs = {}
        self.settings = None
        # Seconds to stall before answering workspace/configuration. A test that wants to
        # reproduce a save landing before the client's settings arrive sets this.
        self.config_delay = 0
        self.pending = {}
        self.notifications = []
        self.requests_from_server = []
        self.lock = threading.Lock()
        self.alive = True
        self.reader = threading.Thread(target=self._read_loop, daemon=True)
        self.reader.start()

    # -- framing ---------------------------------------------------------
    def _read_message(self, timeout=0.5):
        fd = self.proc.stdout.fileno()
        header = b""
        deadline = time.time() + timeout
        while b"\r\n\r\n" not in header:
            if time.time() > deadline:
                return None
            r, _, _ = select.select([fd], [], [], 0.1)
            if not r:
                continue
            chunk = os.read(fd, 1)
            if not chunk:
                return None
            header += chunk
        length = 0
        for line in header.split(b"\r\n"):
            if line.lower().startswith(b"content-length:"):
                length = int(line.split(b":")[1])
        body = b""
        while len(body) < length:
            chunk = os.read(fd, length - len(body))
            if not chunk:
                return None
            body += chunk
        return json.loads(body)

    def _read_loop(self):
        while self.alive:
            try:
                msg = self._read_message(timeout=1.0)
            except Exception:
                break
            if msg is None:
                continue
            if "method" in msg and "id" in msg:
                with self.lock:
                    self.requests_from_server.append(msg)
                self._answer(msg)
            elif "method" in msg:
                with self.lock:
                    self.notifications.append(msg)
            else:
                with self.lock:
                    self.pending[msg.get("id")] = msg

    def _answer(self, msg):
        method = msg["method"]
        if method == "workspace/configuration":
            if self.config_delay:
                time.sleep(self.config_delay)
            section = self.settings if getattr(self, "settings", None) is not None else {}
            result = [section for _ in msg.get("params", {}).get("items", [])]
        elif method == "workspace/applyEdit":
            # A server-initiated edit. A real client applies it to the document it owns and
            # answers with the outcome; that is what a plan application relies on.
            ok, why = self.apply_edit(msg.get("params", {}).get("edit") or {})
            self._write({"jsonrpc": "2.0", "id": msg["id"],
                         "result": {"applied": ok, "failureReason": why}})
            return
        elif method in ("workspace/diagnostic/refresh", "window/workDoneProgress/create"):
            result = None
        else:
            result = None
        self._write({"jsonrpc": "2.0", "id": msg["id"], "result": result})

    # -- document mirror, so an edit the server initiates can be applied for real ---
    @staticmethod
    def _position_to_offset(text, line, character):
        offset, seen = 0, 0
        for i, ch in enumerate(text):
            if seen == line:
                break
            if ch == "\n":
                seen += 1
                offset = i + 1
        return offset + character

    def _apply_text_edits(self, text, edits):
        # Descending order, so an earlier edit cannot shift a later one's offsets.
        placed = []
        for e in edits:
            te = e.get("textEdit", e)
            r = te["range"]
            start = self._position_to_offset(text, r["start"]["line"], r["start"]["character"])
            end = self._position_to_offset(text, r["end"]["line"], r["end"]["character"])
            placed.append((start, end, te["newText"]))
        for start, end, new_text in sorted(placed, key=lambda p: p[0], reverse=True):
            text = text[:start] + new_text + text[end:]
        return text

    def apply_edit(self, edit):
        """Apply a WorkspaceEdit to the mirrored documents and to disk."""
        if not isinstance(edit, dict) or not edit:
            return False, "empty edit"
        touched = set()
        for change in edit.get("documentChanges") or []:
            if "textDocument" in change:
                uri = change["textDocument"]["uri"]
                text = self.docs.get(uri)
                if text is None:
                    return False, f"unknown document {uri}"
                self.docs[uri] = self._apply_text_edits(text, change.get("edits") or [])
                touched.add(uri)
            elif change.get("kind") == "create":
                uri = change["uri"]
                self.docs.setdefault(uri, "")
                touched.add(uri)
        for uri, edits in (edit.get("changes") or {}).items():
            if uri not in self.docs:
                return False, f"unknown document {uri}"
            self.docs[uri] = self._apply_text_edits(self.docs[uri], edits)
            touched.add(uri)
        if not touched:
            return False, "no document changes"
        for uri in touched:
            path = uri[7:] if uri.startswith("file://") else uri
            try:
                with open(path, "w") as fh:
                    fh.write(self.docs[uri])
            except OSError as e:
                return False, str(e)
        return True, None

    def _write(self, payload):
        body = json.dumps(payload).encode()
        self.proc.stdin.write(b"Content-Length: %d\r\n\r\n" % len(body))
        self.proc.stdin.write(body)
        self.proc.stdin.flush()

    # -- api -------------------------------------------------------------
    def notify(self, method, params):
        # Keep a mirror of what we have told the server, so an edit the server initiates can
        # be applied to real content and written back.
        if method == "textDocument/didOpen":
            d = params["textDocument"]
            self.docs[d["uri"]] = d.get("text", "")
        elif method == "textDocument/didChange":
            d = params["textDocument"]
            changes = params.get("contentChanges") or []
            if changes and "range" not in changes[0]:
                self.docs[d["uri"]] = changes[0].get("text", "")
            elif changes and d["uri"] in self.docs:
                self.docs[d["uri"]] = self._apply_text_edits(self.docs[d["uri"]], changes)
        self._write({"jsonrpc": "2.0", "method": method, "params": params})

    def request(self, method, params, timeout=30, poll=0.02):
        """One request, bounded. `poll` is how often the answer is looked for: the default is
        fine for correctness, and a caller that is *measuring* has to pass something finer, or
        every reading is quantised to the poll interval and the bench measures itself."""
        self.next_id += 1
        rid = self.next_id
        self._write({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        deadline = time.time() + timeout
        while time.time() < deadline:
            with self.lock:
                if rid in self.pending:
                    return self.pending.pop(rid)
            time.sleep(poll)
        raise TimeoutError(f"{method} did not answer within {timeout}s")

    def saw_notification(self, method):
        with self.lock:
            return [n for n in self.notifications if n.get("method") == method]

    def saw_request(self, method):
        with self.lock:
            return [r for r in self.requests_from_server if r.get("method") == method]

    def stop(self):
        self.alive = False
        try:
            self.proc.terminate()
            self.proc.wait(timeout=3)
        except Exception:
            self.proc.kill()


RESULTS = []


def expected_anchor(text):
    """The same rule the scripted model applies, so the assertion is not a guess."""
    lines = [
        line for line in text.split("\n")
        if line.strip() and not line.strip().startswith(("//", "#", "*", "/*"))
    ]
    return max(lines, key=len)


def check(ok, label):
    RESULTS.append((bool(ok), label))
    print(("  ok    " if ok else "  FAIL  ") + label)
    return ok


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default=os.path.join(REPO, "target", "release", "meta-lsp"))
    ap.add_argument("--keep", action="store_true", help="do not delete the fixture directory")
    args = ap.parse_args()

    if not os.path.exists(args.bin):
        print(f"server binary not found: {args.bin}", file=sys.stderr)
        return 2

    stub = Stub()
    workdir = tempfile.mkdtemp(prefix="meta-smoke-")
    fixture = os.path.join(workdir, "fixture.rs")
    with open(fixture, "w") as fh:
        fh.write(FIXTURE)
    uri = "file://" + fixture

    env = dict(
        os.environ,
        META_BASE_URL=f"http://127.0.0.1:{stub.port}/v1",
        META_MODEL="stub-model",
        META_REVIEW_MODEL="stub-model",
    )
    server = Lsp([args.bin, "--stdio"], env)
    try:
        print("[smoke] handshake")
        res = server.request("initialize", {
            "processId": os.getpid(),
            "rootUri": "file://" + workdir,
            "capabilities": {"workspace": {"configuration": True, "workspaceFolders": True}},
        })
        caps = res.get("result", {}).get("capabilities", {})
        check(caps.get("positionEncoding") == "utf-8", "server chose utf-8 (N1)")
        check(caps.get("codeActionProvider", {}).get("resolveProvider") is True,
              "codeActionProvider.resolveProvider advertised")
        check(caps.get("diagnosticProvider", {}).get("identifier") == "meta",
              "diagnosticProvider.identifier == meta")
        check(caps.get("executeCommandProvider", {}).get("workDoneProgress") is True,
              "executeCommandProvider.workDoneProgress advertised")
        check(caps.get("inlayHintProvider", {}).get("resolveProvider") is False,
              "inlayHintProvider advertised, fully formed, so no per-hint resolve round trip")
        check(caps.get("codeLensProvider", {}).get("resolveProvider") is False,
              "codeLensProvider advertised, fully formed, so no per-lens resolve round trip")
        # Not just the top-level providers: a sub-capability we do not serve is just as much
        # of a lie, and Neovim routes on this one.
        check(caps.get("diagnosticProvider", {}).get("workspaceDiagnostics") is not True,
              "workspaceDiagnostics is not advertised while only per-document pull is served")
        server.notify("initialized", {})

        print("[smoke] document sync + ambient analysis")
        server.notify("textDocument/didOpen", {"textDocument": {
            "uri": uri, "languageId": "rust", "version": 1, "text": FIXTURE}})
        server.notify("textDocument/didSave", {"textDocument": {"uri": uri}})

        deadline = time.time() + 20
        while time.time() < deadline and not stub.requests()["requests"]:
            time.sleep(0.1)
        reqs = stub.requests()
        check(len(reqs["requests"]) == 1, f"exactly one model call for the analysis (got {len(reqs['requests'])})")
        check(len(server.saw_request("workspace/diagnostic/refresh")) >= 1,
              "server asked the client to re-pull diagnostics")

        diag = server.request("textDocument/diagnostic", {"textDocument": {"uri": uri}})
        report = diag.get("result", {})
        items = report.get("items") or (
            report.get("fullDocumentDiagnosticReport", {}) or {}
        ).get("items", [])
        check(len(items) == 1, f"one finding pulled (got {len(items)})")
        if items:
            d = items[0]
            check(d.get("source") == "meta", "diagnostic source is meta")
            check(d.get("severity") == 2, "finding is a warning, never an error")
            check(isinstance(d.get("data", {}).get("finding_id"), str), "finding carries a stable id")

            # The scripted model anchors on the longest line of the scope; the diagnostic
            # must land on exactly that line and cover exactly that text.
            anchor = expected_anchor(FIXTURE).strip()
            doc_lines = FIXTURE.split("\n")
            want_line = next(i for i, l in enumerate(doc_lines) if anchor in l)
            want_col = doc_lines[want_line].index(anchor)
            got = d.get("range", {})
            check(got.get("start", {}).get("line") == want_line,
                  f"finding landed on the anchor's line {want_line}")
            check(got.get("start", {}).get("character") == want_col
                  and got.get("end", {}).get("character") == want_col + len(anchor),
                  "finding range covers exactly the anchored text")

        print("[smoke] a long file is analysed, not refused for its length")
        # Four hundred lines was the *scope* limit and used to be the file limit too, so any
        # longer file was refused as `over_size` and got no findings, no lenses and no hints —
        # silently, in the ordinary size of module.
        long_path = os.path.join(workdir, "long.rs")
        long_text = "\n".join(
            ["fn long_one() {"] + [f"    let x{i} = {i};" for i in range(600)] + ["}", ""]
        )
        with open(long_path, "w") as fh:
            fh.write(long_text)
        long_uri = "file://" + long_path
        before_calls = len(stub.requests()["requests"])
        server.notify("textDocument/didOpen", {"textDocument": {
            "uri": long_uri, "languageId": "rust", "version": 1, "text": long_text}})
        server.notify("textDocument/didSave", {"textDocument": {"uri": long_uri}})
        deadline = time.time() + 20
        while time.time() < deadline and len(stub.requests()["requests"]) <= before_calls:
            time.sleep(0.1)
        after_calls = len(stub.requests()["requests"])
        check(after_calls > before_calls,
              f"a 600-line file reaches the model ({before_calls} -> {after_calls} calls)")

        print("[smoke] code actions")
        actions = server.request("textDocument/codeAction", {
            "textDocument": {"uri": uri},
            "range": {"start": {"line": 1, "character": 8}, "end": {"line": 1, "character": 8}},
            "context": {"triggerKind": 1, "diagnostics": []},
        }).get("result") or []
        check(len(actions) >= 3, f"a menu is offered (got {len(actions)})")
        check(all(a.get("edit") is None for a in actions), "no action carries an edit on the fast path (N2)")
        check(any(a.get("kind") == "quickfix.meta" for a in actions), "a finding-driven quickfix is offered")
        check(all("data" in a for a in actions), "every action round-trips structured data")
        titles = [a.get("title") for a in actions]
        check(len(set(titles)) == len(titles), "titles are distinct")

        fix = next((a for a in actions if a.get("kind") == "quickfix.meta"), None)
        if not check(fix is not None, "a fix action is available to resolve"):
            return 1

        print("[smoke] resolve")
        resolved = server.request("codeAction/resolve", fix).get("result", {})
        edit = resolved.get("edit")
        check(edit is not None, "resolve returned an edit")
        if edit:
            check("changes" not in edit or edit["changes"] is None, "no bare `changes` map (N4)")
            changes = edit.get("documentChanges") or []
            check(len(changes) >= 1, "documentChanges used")
            first = changes[0]
            check(isinstance(first.get("textDocument", {}).get("version"), int),
                  "textDocument.version is an explicit integer (N4)")

            # `workspace/applyEdit` is a server->client request: the client applies the
            # edit itself. Do exactly that, the way Neovim's apply_workspace_edit does.
            lines = FIXTURE.split("\n")
            for one in first.get("edits", []):
                text_edit = one.get("textEdit", one)
                rng = text_edit["range"]
                if rng["start"]["line"] != rng["end"]["line"]:
                    check(False, "smoke only models single-line replacements")
                    continue
                ln = rng["start"]["line"]
                lines[ln] = (lines[ln][: rng["start"]["character"]]
                             + text_edit["newText"]
                             + lines[ln][rng["end"]["character"]:])
            after = "\n".join(lines)
            check(after != FIXTURE, "the edit, applied by the client, changes the file")
            with open(fixture, "w") as fh:
                fh.write(after)
            check(open(fixture).read() == after, "the applied edit is on disk")
            FIXTURE_AFTER = after
        else:
            FIXTURE_AFTER = FIXTURE

        print("[smoke] staleness")
        server.notify("textDocument/didChange", {
            "textDocument": {"uri": uri, "version": 3},
            "contentChanges": [{"text": FIXTURE_AFTER + "\n// touched\n"}],
        })
        stale = server.request("codeAction/resolve", fix).get("result", {})
        check(stale.get("edit") is None, "a stale action resolves without an edit")
        check(stale.get("disabled", {}).get("reason"), "and says why")

        print("[smoke] commands and progress")
        token = "meta:smoke"
        status = server.request("workspace/executeCommand", {
            "command": "meta.status", "arguments": [], "workDoneToken": token}).get("result", {})
        check(status.get("schema") == "meta.result/1" and status.get("ok") is True, "meta.status returned a result")
        check(status.get("budget", {}).get("calls_last_hour", 0) >= 1, "status reports the calls spent")
        kinds = [n["params"]["value"]["kind"] for n in server.saw_notification("$/progress")
                 if n["params"]["token"] == token]
        check(kinds[:1] == ["begin"] and kinds[-1:] == ["end"],
              f"progress ran begin..end under the client's token (saw {kinds})")

        explain = server.request("workspace/executeCommand", {
            "command": "meta.explain",
            "arguments": [{"uri": uri, "line": 1}],
        }).get("result", {})
        check(explain.get("schema") == "meta.artifact/1" and explain.get("markdown"),
              "meta.explain returned an artifact")

        notimpl = server.request("workspace/executeCommand", {
            "command": "meta.nonexistent", "arguments": []}).get("result", {})
        check(notimpl.get("ok") is False and notimpl.get("error", {}).get("code") == "not_implemented",
              "an unknown command says so instead of pretending")

        bad_args = server.request("workspace/executeCommand", {
            "command": "meta.plan", "arguments": []}).get("result", {})
        check(bad_args.get("ok") is False and bad_args.get("error", {}).get("code") == "bad_arguments",
              f"a served command with missing arguments is refused clearly: {bad_args.get('error')}")

        print("[smoke] shutdown")
        server.request("shutdown", None)
        server.notify("exit", None)
        return 0 if all(ok for ok, _ in RESULTS) else 1
    finally:
        server.stop()
        stub.stop()
        if not args.keep:
            import shutil
            shutil.rmtree(workdir, ignore_errors=True)
        passed = sum(1 for ok, _ in RESULTS if ok)
        print(f"\n[smoke] {passed}/{len(RESULTS)} checks passed")


if __name__ == "__main__":
    sys.exit(main())
