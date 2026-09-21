#!/usr/bin/env python3
"""The supported OpenCode client path: a stdio bridge in front of `jev-lsp --stdio`.

Why this file exists
-------------------
OpenCode 1.18.x as a *native* `jev-lsp` client, measured on this repository (issue #21):

  * it advertises `workspace.diagnostics.refreshSupport = false`, and answers the
    `workspace/diagnostic/refresh` request the server sends anyway with an empty OK — the
    refresh is a no-op;
  * it pulls `textDocument/diagnostic` once when it opens a document, and treats an empty
    report as the answer, so the pull that would have carried the findings is never repeated;
  * the `Diagnostic.report()` it hands the agent after a write keeps only `severity === 1`
    (Error). Every rule finding is a Warning by design, so even a document it did re-pull
    would report nothing to the model.

`jev-lsp`'s finding path is **pull**: ambient analysis finishes, the server sends
`workspace/diagnostic/refresh`, and the client re-pulls `textDocument/diagnostic`
(`PROTOCOL.md` §3.4/§9). That is the surface VS Code and Neovim implement, and it is not
changed here — a server that pushed ambient findings would break §9.

This bridge is a client adapter, not a second product path. It translates the pull contract
into the one OpenCode listens to:

  1. Proxies stdio JSON-RPC between OpenCode and the real `jev-lsp`, untouched apart from
     the four points below.
  2. Answers OpenCode's `initialize` with `diagnosticProvider` removed, so OpenCode takes its
     `textDocument/publishDiagnostics` wait path instead of the pull path it does not finish.
  3. Tells `jev-lsp` `refreshSupport: true` on the way through, and on every
     `workspace/diagnostic/refresh` it answers the server, pulls `textDocument/diagnostic`
     for each open document, and pushes the result to OpenCode as
     `textDocument/publishDiagnostics`.
  4. Remaps severity Warning → Error and prefixes the message `[jev warning]`, because Error
     is the only severity OpenCode's write transcript shows the agent. The prefix is how the
     message stays honest about what the rule said.

OpenCode never sends `textDocument/didSave`, and the rules pass runs on save, so the bridge
synthesizes a save for an open or changed document. Nothing else is invented.

Run it as OpenCode's `lsp.<id>.command`; see `editors/opencode/README.md`.

Environment
-----------
  JEV_LSP_BIN           path to the server binary. Default: `jev-lsp` on `PATH`, else this
                        repository's `target/release/jev-lsp`.
  JEV_OPENCODE_REMAP    `0` leaves rule findings at Warning. Default: remap to Error.

Nothing here touches the network and no API key is read or logged.
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import threading
from pathlib import Path
from typing import Any, Optional
from urllib.parse import unquote, urlparse

LOG_PREFIX = "[jev-opencode-bridge]"
# Warning (2) is what a rule finding carries; OpenCode's agent transcript keeps Error (1) only.
REMAP_WARN_TO_ERROR = os.environ.get("JEV_OPENCODE_REMAP", "1") not in ("0", "false", "no")
WARNING_PREFIX = "[jev warning]"


def log(*parts: object) -> None:
    sys.stderr.write("%s %s\n" % (LOG_PREFIX, " ".join(str(p) for p in parts)))
    sys.stderr.flush()


def resolve_server_binary(explicit: Optional[str]) -> Optional[str]:
    """`--bin`, then `JEV_LSP_BIN`, then `PATH`, then this clone's release build."""
    if explicit:
        return explicit
    env = os.environ.get("JEV_LSP_BIN")
    if env:
        return env
    found = shutil.which("jev-lsp")
    if found:
        return found
    # editors/opencode/<this file> -> repository root
    release = Path(__file__).resolve().parents[2] / "target" / "release" / "jev-lsp"
    return str(release) if release.is_file() else None


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


def uri_to_path(uri: str) -> str:
    return unquote(urlparse(uri).path)


def remap(diagnostics: list) -> list:
    """Warning -> Error with an honest prefix, so OpenCode's agent transcript shows it."""
    out = []
    for diagnostic in diagnostics:
        if not isinstance(diagnostic, dict):
            continue
        item = dict(diagnostic)
        if REMAP_WARN_TO_ERROR and item.get("severity", 2) == 2:
            item["severity"] = 1
            message = item.get("message") or ""
            if not message.startswith(WARNING_PREFIX):
                item["message"] = "%s %s" % (WARNING_PREFIX, message)
        out.append(item)
    return out


class Bridge:
    """One OpenCode connection, one `jev-lsp` child process."""

    def __init__(self, server: list[str]) -> None:
        self.proc = subprocess.Popen(
            server,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=os.environ.copy(),
            bufsize=0,
        )
        assert self.proc.stdin and self.proc.stdout and self.proc.stderr
        self.to_client = sys.stdout.buffer
        self.from_client = sys.stdin.buffer
        self.lock = threading.Lock()
        self.refresh_lock = threading.Lock()
        # The bridge's own requests to the server start high, because the ids a client uses are
        # its own: an id that collides would have one response routed here and the other forwarded
        # to OpenCode as a stray.
        self._id = 900000
        self._pending: dict[int, threading.Event] = {}
        self._results: dict[int, Any] = {}
        self._open: dict[str, dict] = {}
        self._stopped = False
        threading.Thread(target=self._server_reader, daemon=True).start()
        threading.Thread(target=self._stderr_drain, daemon=True).start()

    # -- plumbing ----------------------------------------------------------

    def _stderr_drain(self) -> None:
        assert self.proc.stderr
        for line in self.proc.stderr:
            try:
                sys.stderr.buffer.write(line)
                sys.stderr.buffer.flush()
            except OSError:
                break

    def _send_server(self, message: dict) -> None:
        assert self.proc.stdin
        with self.lock:
            write_frame(self.proc.stdin, message)

    def _send_client(self, message: dict) -> None:
        with self.lock:
            write_frame(self.to_client, message)

    def _server_request(self, method: str, params: Any, timeout: float = 20.0) -> Any:
        with self.lock:
            self._id += 1
            rid = self._id
        event = threading.Event()
        self._pending[rid] = event
        self._send_server({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        if not event.wait(timeout):
            self._pending.pop(rid, None)
            log("no answer to %s within %.0fs" % (method, timeout))
            return None
        return self._results.pop(rid, None)

    # -- server -> client --------------------------------------------------

    def _server_reader(self) -> None:
        assert self.proc.stdout
        while not self._stopped:
            message = read_frame(self.proc.stdout)
            if message is None:
                break
            method = message.get("method")

            if method is None:
                rid = message.get("id")
                if rid in self._pending:
                    self._results[rid] = message.get("result")
                    self._pending[rid].set()
                else:
                    self._send_client(message)  # an answer to a client-originated request
                continue

            if "id" in message:
                if method == "workspace/diagnostic/refresh":
                    # The server asked to be re-pulled. Answer it, then do the pull ourselves
                    # and hand OpenCode the push it actually listens for.
                    self._send_server({"jsonrpc": "2.0", "id": message["id"], "result": None})
                    threading.Thread(target=self._on_refresh, daemon=True).start()
                    continue
                self._send_client(message)  # configuration, progress, applyEdit, ...
                continue

            if method == "textDocument/publishDiagnostics":
                # Legitimate publishes (post-apply verification, PROTOCOL §9) still carry the
                # truth to OpenCode; only the severity is remapped for its transcript.
                params = dict(message.get("params") or {})
                params["diagnostics"] = remap(params.get("diagnostics") or [])
                self._send_client({**message, "params": params})
                continue

            self._send_client(message)

    def _on_refresh(self) -> None:
        with self.refresh_lock:
            for uri in list(self._open):
                result = self._server_request(
                    "textDocument/diagnostic", {"textDocument": {"uri": uri}}, timeout=30.0
                )
                items = result.get("items") if isinstance(result, dict) else None
                diagnostics = remap(items or [])
                params: dict[str, Any] = {"uri": uri, "diagnostics": diagnostics}
                version = self._open.get(uri, {}).get("version")
                if version is not None:
                    params["version"] = version
                self._send_client(
                    {
                        "jsonrpc": "2.0",
                        "method": "textDocument/publishDiagnostics",
                        "params": params,
                    }
                )
                log("pushed %d diagnostic(s) for %s" % (len(diagnostics), Path(uri_to_path(uri)).name))

    # -- client -> server --------------------------------------------------

    def _initialize(self, message: dict) -> None:
        params = dict(message.get("params") or {})
        capabilities = dict(params.get("capabilities") or {})
        workspace = dict(capabilities.get("workspace") or {})
        diagnostics = dict(workspace.get("diagnostics") or {})
        diagnostics["refreshSupport"] = True  # the server must know we will re-pull
        workspace["diagnostics"] = diagnostics
        capabilities["workspace"] = workspace
        params["capabilities"] = capabilities
        result = self._server_request("initialize", params, timeout=60.0)
        if isinstance(result, dict):
            capabilities = dict(result.get("capabilities") or {})
            # OpenCode does not finish the pull path; take it off it. The server is untouched.
            capabilities.pop("diagnosticProvider", None)
            result = {**result, "capabilities": capabilities}
        self._send_client({"jsonrpc": "2.0", "id": message["id"], "result": result})

    def _track_open(self, params: dict) -> Optional[str]:
        document = params.get("textDocument") or {}
        uri = document.get("uri")
        if not uri:
            return None
        self._open[uri] = {"version": document.get("version", 0), "text": document.get("text", "")}
        return uri

    def _track_change(self, params: dict) -> Optional[str]:
        document = params.get("textDocument") or {}
        uri = document.get("uri")
        if not uri:
            return None
        if uri in self._open:
            self._open[uri]["version"] = document.get("version", self._open[uri]["version"])
            changes = params.get("contentChanges") or []
            if len(changes) == 1 and "range" not in changes[0]:
                self._open[uri]["text"] = changes[0].get("text", "")
        return uri

    def _synthesized_save(self, uri: str) -> None:
        """The rules pass is save-triggered and OpenCode never saves: it holds a file open and
        has the agent write it. Without this the ambient pass never runs for OpenCode at all."""
        self._send_server(
            {
                "jsonrpc": "2.0",
                "method": "textDocument/didSave",
                "params": {"textDocument": {"uri": uri}},
            }
        )

    def run(self) -> int:
        while True:
            message = read_frame(self.from_client)
            if message is None:
                break
            method = message.get("method")

            if method is None:
                self._send_server(message)  # the client answering a forwarded request
                continue

            if method == "initialize" and "id" in message:
                self._initialize(message)
                continue

            if method == "textDocument/didOpen":
                uri = self._track_open(message.get("params") or {})
                self._send_server(message)
                if uri:
                    self._synthesized_save(uri)
                continue

            if method == "textDocument/didChange":
                uri = self._track_change(message.get("params") or {})
                self._send_server(message)
                if uri:
                    self._synthesized_save(uri)
                continue

            if method == "textDocument/didClose":
                document = (message.get("params") or {}).get("textDocument") or {}
                self._open.pop(document.get("uri"), None)

            if method == "exit":
                self._send_server(message)
                break

            self._send_server(message)

        self._stopped = True
        try:
            self.proc.terminate()
            self.proc.wait(timeout=5)
        except Exception:
            self.proc.kill()
        return 0


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(
        description="Bridge OpenCode 1.18's diagnostics path to a `jev-lsp --stdio` server."
    )
    parser.add_argument("--bin", help="path to jev-lsp (default: JEV_LSP_BIN, PATH, then target/release)")
    args = parser.parse_args(argv)

    server = resolve_server_binary(args.bin)
    if not server or not Path(server).is_file():
        log("no jev-lsp binary found (--bin, JEV_LSP_BIN, PATH, target/release/):", server or "none")
        return 2
    log("bridging OpenCode to", server, "--stdio")
    return Bridge([server, "--stdio"]).run()


if __name__ == "__main__":
    sys.exit(main())
