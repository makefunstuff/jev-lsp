#!/usr/bin/env python3
"""A client shaped like OpenCode 1.18, driven at a `jev-lsp` server or at the bridge.

This is the focused check behind `verify-bridge.sh` and the evidence for issue #21. It is not
a general LSP client: it does the four things OpenCode 1.18 does, and nothing else.

  1. `initialize` with `workspace.diagnostics.refreshSupport = false`, and answers the
     `workspace/diagnostic/refresh` the server sends anyway with an empty OK.
  2. If the answer advertises `diagnosticProvider`, it pulls `textDocument/diagnostic` **once**
     on open and treats that report — empty or not — as the answer. It never pulls again.
  3. `textDocument/didOpen`, and no `didSave` ever (`synchronization.didSave` is false).
  4. It surfaces what arrives by `textDocument/publishDiagnostics`.

`--expect surfaced` is the bridge's claim: a rule finding reaches this client. `--expect empty`
is the native flake reproduced: the same fixture, the same server, no bridge, and the client
finishes with nothing to show. Both are checked, so "surfaced" cannot be an artifact of the
fixture failing to produce a finding at all.

Usage (verify-bridge.sh does this; the arguments are spelled out in `editors/opencode/README.md`):

  opencode_probe.py --bridge <bridge.py> --bin <jev-lsp> --root <dir> --file handler.rs \
      --stub-url http://127.0.0.1:8098/v1 --expect surfaced [--timeout 40] [--json-out f.json]
  opencode_probe.py --native <jev-lsp> --root <dir> --file handler.rs --expect empty

Exit: 0 expectation met, 1 not met, 2 the run could not happen. The JSON record is on stdout.
Standard library only.
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Any, Optional
from urllib.parse import quote, urlparse

WARNING_PREFIX = "[jev warning]"
SETTLE_S = 0.4


def log(*parts: object) -> None:
    sys.stderr.write("[probe] " + " ".join(str(p) for p in parts) + "\n")
    sys.stderr.flush()


def read_frame(stream) -> Optional[dict]:
    headers: dict[str, str] = {}
    while True:
        line = stream.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        if b":" in line:
            key, value = line.decode("utf-8", errors="replace").split(":", 1)
            headers[key.strip().lower()] = value.strip()
    length = int(headers.get("content-length", "0"))
    if length <= 0:
        return None
    body = stream.read(length)
    if len(body) < length:
        return None
    return json.loads(body.decode("utf-8"))


def write_frame(stream, message: dict) -> None:
    body = json.dumps(message, ensure_ascii=False).encode("utf-8")
    stream.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    stream.flush()


def file_uri(path: Path) -> str:
    return "file://" + quote(str(path))


def language_id(path: Path) -> str:
    return {
        ".rs": "rust", ".py": "python", ".ts": "typescript", ".tsx": "typescriptreact",
        ".js": "javascript", ".go": "go", ".md": "markdown", ".toml": "toml",
    }.get(path.suffix, "plaintext")


def opencode_capabilities() -> dict:
    """What OpenCode 1.18 advertises, as far as this path cares. `didSave` is false on purpose:
    it is a notification OpenCode does not send, and the rules pass is save-triggered."""
    return {
        "general": {"positionEncodings": ["utf-8"]},
        "window": {"workDoneProgress": True, "showMessage": {}, "showDocument": {"support": True}},
        "workspace": {
            "applyEdit": True,
            "configuration": True,
            "workspaceFolders": True,
            "didChangeWatchedFiles": {"dynamicRegistration": True},
            "diagnostics": {"refreshSupport": False},
        },
        "textDocument": {
            "synchronization": {"dynamicRegistration": False, "didSave": False},
            "publishDiagnostics": {"versionSupport": True},
            "diagnostic": {"dynamicRegistration": False, "relatedDocumentSupport": False},
        },
    }


def jev_settings(stub_url: str) -> dict:
    """A configured `jev` section: the repository's own rules, the shipped set off (so the
    fixture decides what fires), and the decide tier on the stub."""
    tier = {
        "wire": "system_one",
        "base_url": stub_url,
        "model": "stub",
        "temperature": 0.0,
        "timeout_ms": 15000,
    }
    return {
        "enabled": True,
        "rules": {"enabled": True, "defaults": False},
        "ambient": {"diagnostics": True},
        "models": {"decide": tier, "reason": dict(tier), "review": dict(tier)},
        "auto_apply": {"fix": False, "fixAll": False},
    }


class Probe:
    def __init__(self, command: list[str], root: Path, settings: dict) -> None:
        self.proc = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=os.environ.copy(),
            bufsize=0,
        )
        assert self.proc.stdin and self.proc.stdout and self.proc.stderr
        self.root = root
        self.settings = settings
        self._id = 0
        self._lock = threading.Lock()
        self._cv = threading.Condition(self._lock)
        self._pending: dict[int, dict] = {}
        self._publishes: list[dict] = []
        self._server_requests: list[dict] = []
        self._stderr: list[str] = []
        threading.Thread(target=self._reader, daemon=True).start()
        threading.Thread(target=self._drain_stderr, daemon=True).start()

    # -- transport ---------------------------------------------------------

    def _send(self, message: dict) -> None:
        assert self.proc.stdin
        with self._lock:
            write_frame(self.proc.stdin, message)

    def _drain_stderr(self) -> None:
        assert self.proc.stderr
        for line in self.proc.stderr:
            text = line.decode("utf-8", errors="replace").rstrip("\n")
            self._stderr.append(text)
            log("stderr:", text)

    def _reader(self) -> None:
        assert self.proc.stdout
        while True:
            message = read_frame(self.proc.stdout)
            if message is None:
                with self._cv:
                    self._cv.notify_all()
                return
            method = message.get("method")
            if method is not None and "id" in message:
                self._server_requests.append({"method": method, "params": message.get("params")})
                self._answer_server_request(message)
            elif method is not None:
                if method == "textDocument/publishDiagnostics":
                    with self._cv:
                        self._publishes.append(message.get("params") or {})
                        self._cv.notify_all()
                elif method == "window/logMessage":
                    log("server:", (message.get("params") or {}).get("message"))
            else:
                with self._cv:
                    entry = self._pending.get(message.get("id"))
                    if entry is not None:
                        entry["result"] = message.get("result")
                        entry["error"] = message.get("error")
                        entry["done"] = True
                    self._cv.notify_all()

    def _answer_server_request(self, message: dict) -> None:
        method = message["method"]
        params = message.get("params") or {}
        if method == "workspace/configuration":
            items = params.get("items") or []
            result = [self.settings if (item or {}).get("section") == "jev" else None for item in items]
        elif method == "window/showDocument":
            result = {"success": True}
        elif method == "window/showMessageRequest":
            result = None
        elif method in ("workspace/diagnostic/refresh", "workspace/codeLens/refresh",
                        "workspace/inlayHint/refresh", "client/registerCapability",
                        "client/unregisterCapability", "window/workDoneProgress/create"):
            # OpenCode answers `workspace/diagnostic/refresh` like this: an empty OK, and nothing
            # follows. The re-pull the server is asking for is exactly what never happens.
            result = None
        else:
            self._send({"jsonrpc": "2.0", "id": message["id"],
                        "error": {"code": -32601, "message": method}})
            return
        self._send({"jsonrpc": "2.0", "id": message["id"], "result": result})

    def request(self, method: str, params: Any, timeout: float = 60.0) -> Any:
        with self._lock:
            self._id += 1
            rid = self._id
            entry = {"done": False}
            self._pending[rid] = entry
        self._send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        deadline = time.monotonic() + timeout
        with self._cv:
            while not entry["done"] and time.monotonic() < deadline:
                self._cv.wait(0.2)
        if not entry["done"]:
            raise TimeoutError("%s did not answer within %.0fs" % (method, timeout))
        if entry.get("error"):
            raise RuntimeError("%s failed: %s" % (method, entry["error"]))
        return entry["result"]

    def notify(self, method: str, params: Any) -> None:
        self._send({"jsonrpc": "2.0", "method": method, "params": params})

    # -- what the probe does ----------------------------------------------

    def wait_for_push(self, uri: str, timeout: float) -> None:
        deadline = time.monotonic() + timeout
        with self._cv:
            while time.monotonic() < deadline:
                if any(p.get("uri") == uri for p in self._publishes):
                    break
                self._cv.wait(0.2)
            # One more settle window: a second push (a later pass) should not be missed by the
            # check, and it costs a bounded 0.4 s rather than a race.
            self._cv.wait(SETTLE_S)

    def close(self) -> None:
        try:
            self._send({"jsonrpc": "2.0", "id": 999999, "method": "shutdown", "params": None})
        except Exception:
            pass
        try:
            self.proc.terminate()
            self.proc.wait(timeout=5)
        except Exception:
            self.proc.kill()

    def run(self, relative: str, timeout: float) -> dict:
        path = (self.root / relative).resolve()
        uri = file_uri(path)
        text = path.read_text(encoding="utf-8")
        started = time.monotonic()

        init = self.request(
            "initialize",
            {
                "processId": os.getpid(),
                "clientInfo": {"name": "opencode-probe", "version": "1.18"},
                "rootUri": file_uri(self.root),
                "workspaceFolders": [{"uri": file_uri(self.root), "name": self.root.name}],
                "capabilities": opencode_capabilities(),
                "initializationOptions": {"jev": self.settings},
            },
        )
        capabilities = (init or {}).get("capabilities") or {}
        provider_advertised = "diagnosticProvider" in capabilities
        self.notify("initialized", {})

        pulls: list[dict] = []
        self.notify(
            "textDocument/didOpen",
            {
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id(path),
                    "version": 1,
                    "text": text,
                }
            },
        )
        if provider_advertised:
            # OpenCode's pull path, taken exactly once, right here. An empty report at this
            # moment — the ambient pass has not finished — is the answer it keeps for the whole
            # session, and the reason nothing reaches the agent. The fixture's decision call is
            # stalled (STUB_DELAY_MS) so this is the situation, not a race won by luck.
            report = self.request("textDocument/diagnostic", {"textDocument": {"uri": uri}})
            pulls.append(report if isinstance(report, dict) else {})
            log("pulled once on open; %d item(s)" % len((pulls[-1].get("items") or [])))

        self.wait_for_push(uri, timeout)
        self.close()

        pushed = [p for p in self._publishes if p.get("uri") == uri]
        diagnostics = pushed[-1].get("diagnostics") if pushed else []
        remapped = [d for d in (diagnostics or []) if str(d.get("message") or "").startswith(WARNING_PREFIX)]
        return {
            "provider_advertised": provider_advertised,
            "pulls": len(pulls),
            "first_pull_items": len((pulls[0].get("items") or [])) if pulls else None,
            "refresh_requests_acked": sum(
                1 for r in self._server_requests if r["method"] == "workspace/diagnostic/refresh"
            ),
            "pushes": len(pushed),
            "diagnostics": diagnostics or [],
            "severities": sorted({d.get("severity", 2) for d in (diagnostics or [])}),
            "jev_warning_prefixed": len(remapped),
            "surfaced": bool(diagnostics),
            "elapsed_ms": int((time.monotonic() - started) * 1000),
        }


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--native", metavar="JEV_LSP", help="drive jev-lsp --stdio directly")
    mode.add_argument("--bridge", metavar="BRIDGE", help="drive the bridge in front of jev-lsp")
    parser.add_argument("--bin", help="path to jev-lsp (default: JEV_LSP_BIN, then PATH)")
    parser.add_argument("--root", default=".", help="workspace root the server is opened on")
    parser.add_argument("--file", default="handler.rs", help="file inside --root to open")
    parser.add_argument("--stub-url", default="http://127.0.0.1:8098/v1",
                        help="decide endpoint (an OpenAI-shaped stub) the jev section names")
    parser.add_argument("--timeout", type=float, default=40.0,
                        help="seconds to wait for a publish after didOpen")
    parser.add_argument("--expect", choices=["surfaced", "empty"], required=True)
    parser.add_argument("--json-out", help="write the full record here as well")
    args = parser.parse_args(argv)

    server = args.bin or os.environ.get("JEV_LSP_BIN") or "jev-lsp"
    if args.native:
        if not Path(args.native).is_file():
            log("no server binary at", args.native)
            return 2
        command = [args.native, "--stdio"]
        mode_name = "native"
    else:
        if not Path(args.bridge).is_file():
            log("no bridge at", args.bridge)
            return 2
        command = [sys.executable, str(Path(args.bridge).resolve()), "--bin", server]
        mode_name = "bridge"

    root = Path(args.root).resolve()
    probe = Probe(command, root, jev_settings(args.stub_url))
    try:
        record = probe.run(args.file, args.timeout)
    except (TimeoutError, RuntimeError) as error:
        log("FAIL", mode_name, error)
        return 1
    record["mode"] = mode_name
    record["command"] = command

    if args.expect == "surfaced":
        # The claim under test: a rule finding reaches a client that behaves like OpenCode,
        # carrying the severity its agent transcript keeps and the prefix that keeps the
        # message honest.
        ok = (
            not record["provider_advertised"]
            and record["surfaced"]
            and 1 in record["severities"]
            and record["jev_warning_prefixed"] >= 1
        )
        claim = "a finding reaches an OpenCode-shaped client (Error severity, [jev warning] prefix)"
    else:
        # The flake reproduced: the native path, the same fixture, an empty pull and no push.
        ok = (
            record["provider_advertised"]
            and record["pulls"] == 1
            and record["first_pull_items"] == 0
            and not record["surfaced"]
        )
        claim = "the native path leaves an OpenCode-shaped client with nothing (issue #21)"

    print(json.dumps(record, indent=2, sort_keys=True))
    if args.json_out:
        Path(args.json_out).write_text(json.dumps(record, indent=2, sort_keys=True) + "\n",
                                       encoding="utf-8")
    log("%s %s: %s" % ("PASS" if ok else "FAIL", mode_name, claim))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
