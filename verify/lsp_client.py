#!/usr/bin/env python3
"""verify/lsp_client.py — the independent stdio LSP client (docs/VERIFICATION.md §1).

Purpose
-------
Speak the frozen contract in PROTOCOL.md to a `jev-lsp` server over stdio and assert it,
step by step, sharing no code with the server. A disagreement between this client and the
server is a protocol defect, not a test artifact: every assertion below is transcribed from
PROTOCOL.md / docs/VERIFICATION.md §1, and there is no server-side helper to drift against.

Runs, in order, and asserts at each step (docs/VERIFICATION.md §1):

  1. `initialize` -> `initialized`; `positionEncoding == "utf-8"` (N1),
     `codeActionProvider.resolveProvider == true`, `diagnosticProvider.identifier == "jev"`,
     `executeCommandProvider.workDoneProgress == true`, the command set of §6 read from the
     contract and matched to the capability block in both directions (nothing advertised that
     §6 does not name, nothing §6 serves left unadvertised), and no draft capability advertised
     — §2's "advertise only what is served" is the rule the set is checked against.
  2. `textDocument/didOpen` for two fixture documents written into a temp dir under the
     workspace (a normal `.py` and a never-touched control `.txt`). A notification carries
     no assertion of its own; the control document exists to make step 9 checkable.
  3. `textDocument/codeAction`, `triggerKind = 1` (Invoked): the response arrives within the
     50 ms budget (docs/ARCHITECTURE.md §4) and no returned action carries an `edit` (N2).
  4. `textDocument/codeAction`, `triggerKind = 2` (Automatic): every returned action has
     `data.state == "ready"` — never a `pending` placeholder (§3.1).
  5. `codeAction/resolve` on the first action: `edit.documentChanges` is present, no bare
     `changes` key, every `TextDocumentEdit.textDocument.version` is an integer (§8, N4).
  6. `workspace/applyEdit` with that edit: `applied == true`.
  7. Staleness: take `codeAction` again, mutate the document with `didChange`, resolve the
     previously obtained action -> no `edit` in the response (§8 rule 3) and no JSON-RPC
     error.
  8. `textDocument/diagnostic`: after `didSave` the server's background pass asks to be
     re-pulled with `workspace/diagnostic/refresh` (§3.4/§9); the re-pull must carry
     findings, `kind == "full"`, a `resultId`, and `data.finding_id` + `data.verb` on every
     item (§9). An empty report after that refresh is a FAIL — a correct-but-empty pull
     before it is exactly the mistake a real client must not make.
  9. `workspace/executeCommand` with a `workDoneToken` in the params, on a served command:
     a `$/progress` `begin` and an `end` arrive for that token (§3.5), nothing arrives under
     that token after the `end` — the token is closed, read after a settle window — no
     `$/progress` arrives under a token this client never supplied or created (§3.5), the two
     failure paths stay distinct — an unserved `jev.nonexistent` answers `not_implemented`, a
     served `jev.plan` with unusable arguments answers `bad_arguments` (§6.1) — and no
     `textDocument/publishDiagnostics` arrives for a document the server has not changed
     (§9).
 10. The other three §3.5 paths, each in a server of its own (see the Step 10 note below):
     a chat tier pointing at an endpoint nothing answers on, a budget of zero calls a
     minute, and a cancellation delivered mid-model-call. Each must produce exactly one
     `begin` and exactly one `end` under the token this client supplied, its own documented
     failure, and a server that answers the next command afterwards.
 11. Every advertised command, called on an open document: each must answer something other
     than `not_implemented` (§2: an advertised command the server does not implement is a
     promise it breaks).

Three readings the frozen documents leave open, settled here and recorded so a reviewer can
challenge them:

  * Step 6 direction. `workspace/applyEdit` is a server->client request (PROTOCOL §3.4);
    `applied` is the *client's* answer, not the server's. So this client answers such
    requests (see `Session._handle_server_request`) and, for step 6, applies the edit from
    step 5 to the documents this client owns through that same code path, asserting
    `applied == true`, then syncing the result with `textDocument/didChange` — which is
    exactly what Neovim does with `vim.lsp.util.apply_workspace_edit` (see
    verify/probes/trace.lua). It never sends `workspace/applyEdit` *to* the server: a
    client->server request under that name is not in the specification, and a client that
    invented one would report a defect where there is none.
  * Step 9 command. PROTOCOL §3.5's normal path is a `workDoneToken` inside the
    `workspace/executeCommand` params, and PROTOCOL §6 serves every command it lists, so the
    progress assertions are driven on `jev.status` when it is advertised (else the first
    advertised command) — a served command that answers immediately still owes the request
    its `begin` and `end`.
    docs/VERIFICATION.md §1 words this step as "`jev.cancel` mid-flight"; that is the same
    assertion over a different command — §3.5's token rule, checked as `begin` and `end`
    under the token this request supplied — and the ticket specifies the token form, so the
    token form is what is driven here. `jev.cancel` is the §6 command for work the plugin
    did not issue; driving a cancel against a request we are concurrently awaiting would
    test `$/cancelRequest`, which docs/VERIFICATION.md does not ask this client for.
  * Step 10's servers, and the one path it does not drive. A path whose failure *is* the
    configuration (an unreachable tier, a budget of zero) cannot be driven through the
    session above, which is configured once against the suite's stub; so each of the three
    starts its own `jev-lsp` in its own temp workspace, and each answers
    `workspace/configuration` with exactly the settings that path needs. The **panic** path
    is deliberately not here: the only way to panic a command over the wire is a command the
    product does not have, and a test-only command behind an environment variable is still a
    shipped way to crash a command. It stays pinned by the unit test
    `server::progress_tests::a_panic_between_begin_and_end_still_sends_its_end` in
    `crates/jev-lsp/src/server.rs`, and PROTOCOL §3.5 names the gap rather than hiding it.
  * The `end`/response order. §3.5 gives the token its lifetime — "It is valid only until the
    response to that request is sent" — and the server keeps it in its own order: the `end` is
    handed to the transport before the command body returns, so before the response exists. It
    does not order the two *on the wire*: `tower-lsp` writes notifications and responses through
    two arms of one `futures::stream::select`, which polls them round-robin, so when both are
    ready in the same poll the arm that wins is whichever side the round-robin reached. Measured
    against one unchanged binary: green at `end 207.431844719, response 207.431873793`; red at
    `end 157.797434027, response 157.797412763`. Step 9 and step 10 therefore record that
    interleaving and assert what the process controls instead — one `begin`, one `end`, and
    nothing under the token after the `end`, read after a settle window, so a leak that came out
    after the first shape read is still caught.

Statuses, printed per assertion:
  ok     the assertion held
  FAIL   the assertion did not hold — fails the run
  skip   could not be asserted (e.g. no actions were offered); does not fail the run, but
         is always listed in the summary. Never counted as a pass.
  warn   a contract observation that is not an assertion and cannot fail the run (e.g. the
         server offered nothing on an Invoked trigger, where §3.1 expects one `disabled`
         placeholder on a cold cache).
  info   a step with nothing to assert, or context for the lines that follow.

Exit codes: 0 every assertion passed; 1 at least one FAIL; 2 harness error (no server,
broken transport, bad usage).

Standard library only, on purpose, and **Python 3.10 or newer**: the fixtures are written with
`pathlib.Path.write_text(newline=…)`, which on 3.9 raises `TypeError` before the first assertion
runs — a red that names nothing. The in-process stub at the bottom exists solely for
`--selftest`, which proves the client — framing, request/response correlation, the nine steps
a stub can serve, and that twelve injected contract defects each turn the harness red — before
the Rust server exists. The stub's command set is §6's, read from the contract like the
harness's, so step 11 covers it too. Step 10 needs servers configured three different ways, so it is not
reachable from the in-process stub; it reports that rather than pretending to have run.
"""

import argparse
import json
import os
import pathlib
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import traceback
import urllib.request
from urllib.parse import unquote, urlparse

# --------------------------------------------------------------------------- constants

DEFAULT_TIMEOUT = 20.0          # per request, seconds (--timeout)
BUDGET_CODE_ACTION_MS = 50.0    # docs/ARCHITECTURE.md §4: codeAction p99 < 50 ms
PROGRESS_TOKEN = "jev:verify-1"
PROGRESS_WAIT_S = 5.0           # grace after the response for §3.5's begin/end, bounded
FINDINGS_WAIT_S = 20.0          # grace for the §3.4 `workspace/diagnostic/refresh` the
                                # server sends when its background pass lands, bounded by
                                # the resolve cap in ARCHITECTURE §4 (p50 < 2 s, cap 30 s)
FINDINGS_GRACE_S = 2.0          # re-pull grace after that refresh: the pass is already done
SETTLE_NEGATIVE_S = 0.25        # window for a negative (absence) wait

# PROTOCOL §4.1 — the frozen verb set. Findings name the verb that fixes them (§9).
VERBS = ("fix", "harden", "types", "docs", "rewrite", "test", "explain", "review",
         "generate", "fixAll")
ACTION_STATES = ("ready", "pending", "stale", "over_budget", "failed")

FIXTURE_PY = """\
import json


def parse_retry(payload, attempts=3):
    \"\"\"Parse a JSON payload, retrying a few times.\"\"\"
    for attempt in range(attempts):
        try:
            return json.loads(payload)
        except ValueError:
            continue
    return None
"""

FIXTURE_MARKER = "for attempt in range(attempts)"

FIXTURE_TXT = """\
Scratch notes. This document is opened and never modified: it is the control for the
unsolicited `textDocument/publishDiagnostics` assertion (PROTOCOL §9).
"""


class HarnessError(Exception):
    """This client could not run the harness at all (bad usage, dead transport)."""


class FramingError(Exception):
    """Bytes on the wire did not form a Content-Length framed JSON-RPC message."""


class TransportError(Exception):
    """The server's stream ended or failed mid-session."""


class EditRefused(Exception):
    """A WorkspaceEdit was refused by the client's §8 validator."""


class RequestTimeout(Exception):
    """No response inside the per-request budget. Raised, never hung on."""


class ServerError(Exception):
    """A JSON-RPC error response."""

    def __init__(self, method, error):
        self.method = method
        self.error = error if isinstance(error, dict) else {"code": None, "message": str(error)}
        Exception.__init__(self, "server error for %s: %s" % (method, json.dumps(self.error)))

    @property
    def code(self):
        return self.error.get("code")


# --------------------------------------------------------------------------- transport


def read_message(rfile):
    """Read one Content-Length framed JSON-RPC message. None at EOF.

    Framing is byte-counted, not character-counted: non-ASCII payloads are the case that
    separates a correct client from one that measures `len(str)`.
    """
    length = None
    while True:
        line = rfile.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        name, _, value = line.partition(b":")
        if name.strip().lower() == b"content-length":
            try:
                length = int(value.strip())
            except ValueError:
                raise FramingError("unparsable Content-Length: %r" % (line,))
    if length is None:
        raise FramingError("headers ended without Content-Length")
    body = rfile.read(length)
    if body is None or len(body) != length:
        raise FramingError("short body: wanted %d bytes, got %s" %
                           (length, "EOF" if not body else len(body)))
    try:
        return json.loads(body.decode("utf-8"))
    except (UnicodeDecodeError, ValueError) as exc:
        raise FramingError("body was not UTF-8 JSON: %s" % exc)


def write_message(wfile, payload):
    """Write one Content-Length framed JSON-RPC message."""
    body = json.dumps(payload, separators=(",", ":")).encode("utf-8")
    wfile.write(b"Content-Length: %d\r\n\r\n" % len(body))
    wfile.write(body)
    wfile.flush()


def _rpc_message(method, params=None, msg_id=None):
    """Build a request or a notification, omitting `params` entirely when there is none.

    JSON-RPC 2.0 says `params` MAY be omitted and must be an object/array when present, so
    `"params": null` is not a legal member. LSP's `shutdown` and `exit` take no params, and
    servers that read the member strictly answer -32602 "Unexpected params: null" — the real
    `jev-lsp` does, and Neovim's own client omits the member (`lsp/client.lua:911` calls
    `rpc.request('shutdown', nil, …)`; Lua drops the nil, so the key never reaches the wire).
    This is an interop trap, not a style choice.
    """
    message = {"jsonrpc": "2.0", "method": method}
    if msg_id is not None:
        message["id"] = msg_id
    if params is not None:
        message["params"] = params
    return message


def log(*parts):
    """Diagnostics go to stderr; stdout carries nothing but the report."""
    print("[lsp_client]", *parts, file=sys.stderr, flush=True)


def _token_key(token):
    if isinstance(token, bool) or not isinstance(token, (str, int)):
        return json.dumps(token, sort_keys=True)
    return token


def _line_starts(text):
    starts = [0]
    index = text.find("\n")
    while index != -1:
        starts.append(index + 1)
        index = text.find("\n", index + 1)
    return starts


def _uri_to_path(uri):
    parsed = urlparse(uri)
    if parsed.scheme != "file":
        raise EditRefused("resource operation on a non-file uri: %s" % uri)
    return unquote(parsed.path)


def client_capabilities():
    """What this client advertises. `positionEncodings` is pinned to utf-8 so the server's
    choice in §2 is negotiated rather than assumed [R6]."""
    kinds = ["quickfix", "quickfix.jev", "refactor.rewrite", "refactor.rewrite.jev",
             "source", "source.jev", "source.fixAll"]
    return {
        "general": {"positionEncodings": ["utf-8"], "markdown": {"parser": "none"}},
        "window": {"workDoneProgress": True,
                   "showMessage": {},
                   "showDocument": {"support": True}},
        "workspace": {
            "applyEdit": True,
            "configuration": True,
            "workspaceFolders": True,
            "workspaceEdit": {"documentChanges": True,
                              "resourceOperations": ["create", "rename", "delete"],
                              "failureHandling": "abort"},
            "didChangeWatchedFiles": {"dynamicRegistration": True},
            "diagnostics": {"refreshSupport": True},
            "codeLens": {"refreshSupport": True},
        },
        "textDocument": {
            "synchronization": {"dynamicRegistration": False, "didSave": True},
            "publishDiagnostics": {"versionSupport": True},
            "codeAction": {
                "dynamicRegistration": False,
                "isPreferredSupport": True,
                "disabledSupport": True,
                "dataSupport": True,
                "resolveSupport": {"properties": ["edit", "command"]},
                "codeActionLiteralSupport": {"codeActionKind": {"valueSet": kinds}},
            },
            "diagnostic": {"dynamicRegistration": False, "relatedDocumentSupport": False},
            "hover": {"contentFormat": ["markdown", "plaintext"]},
            "inlayHint": {"dynamicRegistration": False,
                          "resolveSupport": {"properties": ["text", "tooltip", "location"]}},
        },
    }


# --------------------------------------------------------------------------- session


class Session(object):
    """An LSP client session over two binary streams.

    One reader thread owns the read side: it resolves responses by id (never by order),
    records notifications in arrival order, and answers the server->client requests this
    client understands. Every wait is bounded, so a silent server fails a step instead of
    hanging the harness.
    """

    def __init__(self, rfile, wfile, timeout=DEFAULT_TIMEOUT, name="server",
                 workspace_folders=None, process=None):
        self._rfile = rfile
        self._wfile = wfile
        self.timeout = timeout
        self.name = name
        self.workspace_folders = workspace_folders or []
        self.save_include_text = False  # set from the server's textDocumentSync.save
        self.stub_model_url = None
        self.settings = None        # step 10: the exact `jev` section this session answers
                                    # `workspace/configuration` with, when a path under test
                                    # needs a server configured differently from the stub
        self.stub = None            # in-process stub, for --selftest teardown only
        self._process = process     # subprocess, when the client spawned a real server
        self._last_response_t = None
        self.timeout_streak = 0     # consecutive per-request timeouts, for _try_request

        self._wlock = threading.Lock()
        self._cv = threading.Condition(threading.RLock())
        self._next_id = 0
        self._pending = {}
        self._messages = []         # notifications, arrival ordered
        self._server_requests = []
        self._stray_responses = []  # responses whose id matched nothing we sent
        self._owned_tokens = set()  # workDoneToken we supplied, or the server created
        self._changed = set()       # uris whose text this client has changed
        self._change_log = []       # [(monotonic time, uri)] — §9 allows a publish only
                                    # after the server's own change to that document
        self._docs = {}             # uri -> {version, text, path, language_id}
        self._doclock = threading.RLock()
        self._fatal = None

        self._thread = threading.Thread(target=self._read_loop, name="lsp-reader", daemon=True)
        self._thread.start()

    # -- reader ------------------------------------------------------------

    def _read_loop(self):
        try:
            while True:
                msg = read_message(self._rfile)
                if msg is None:
                    raise EOFError("server closed its stdout")
                self._dispatch(msg)
        except Exception as exc:                       # noqa: BLE001 — reported, not raised here
            with self._cv:
                self._fatal = exc
                for entry in self._pending.values():
                    if not entry["done"]:
                        entry["error"] = TransportError("stream died: %s" % exc)
                        entry["done"] = True
                        entry["event"].set()
                self._pending.clear()
                self._cv.notify_all()

    def _dispatch(self, msg):
        if not isinstance(msg, dict):
            raise FramingError("message was not a JSON object: %r" % (msg,))
        if "method" in msg and "id" in msg:
            self._handle_server_request(msg)
        elif "method" in msg:
            with self._cv:
                self._messages.append({"method": msg["method"], "params": msg.get("params"),
                                       "t": time.monotonic()})
                self._cv.notify_all()
        elif "id" in msg:
            self._settle(msg)
        else:
            raise FramingError("message carried neither method nor id: %r" % (msg,))

    def _settle(self, msg):
        rid = msg["id"]
        with self._cv:
            self._last_response_t = time.monotonic()
            entry = self._pending.pop(rid, None)
            if entry is None:
                self._stray_responses.append(msg)
                log("response for an id we never sent:", rid)
                return
            if "error" in msg:
                entry["error"] = ServerError(entry["method"], msg["error"])
            else:
                entry["result"] = msg.get("result")
            entry["t"] = time.monotonic()
            entry["done"] = True
            entry["event"].set()

    def _handle_server_request(self, msg):
        method = msg["method"]
        params = msg.get("params") or {}
        rid = msg["id"]
        with self._cv:
            self._server_requests.append({"method": method, "params": params})

        if method == "workspace/configuration":
            items = params.get("items") or []
            self._write({"jsonrpc": "2.0", "id": rid,
                         "result": [self._configuration_value(item) for item in items]})
        elif method == "window/workDoneProgress/create":
            token = params.get("token")
            if token is not None:
                self._owned_tokens.add(_token_key(token))
            self._write({"jsonrpc": "2.0", "id": rid, "result": None})
        elif method == "workspace/applyEdit":
            result = self.apply_workspace_edit(params.get("edit") or {})
            self._write({"jsonrpc": "2.0", "id": rid, "result": dict(result)})
        elif method in ("workspace/diagnostic/refresh", "workspace/codeLens/refresh",
                        "client/registerCapability", "client/unregisterCapability"):
            self._write({"jsonrpc": "2.0", "id": rid, "result": None})
        elif method == "workspace/workspaceFolders":
            self._write({"jsonrpc": "2.0", "id": rid, "result": self.workspace_folders})
        elif method == "window/showMessageRequest":
            self._write({"jsonrpc": "2.0", "id": rid, "result": None})
        elif method == "window/showDocument":
            self._write({"jsonrpc": "2.0", "id": rid, "result": {"success": True}})
        else:
            log("unhandled server->client request %s; replying MethodNotFound" % method)
            self._write({"jsonrpc": "2.0", "id": rid,
                         "error": {"code": -32601,
                                   "message": "unhandled client-side method: %s" % method}})

    def _configuration_value(self, item):
        section = (item or {}).get("section")
        if section == "jev":
            return self._jev_config()
        return None

    def _jev_config(self):
        """PROTOCOL §10: configuration travels over workspace/configuration, and no
        environment variable beyond the model endpoints exists. With `--stub-model-url`
        the client answers as a configured plugin would, pointing every tier at the stub.

        `rules.enabled` is off so the ambient pass is the chat review this client is written
        against: the repository's own `.jev/rules/*.json` are a separate surface with their own
        claims (`verify/rules_test.py`), and this client checks that a pull carries *a*
        conclusion, not which pass wrote it."""
        if self.settings is not None:
            # Step 10 only: a path whose failure is the configuration needs a server whose
            # configuration is exactly this. Nothing else sets it.
            return self.settings
        if not self.stub_model_url:
            return {"rules": {"enabled": False}}
        tier = {"base_url": self.stub_model_url, "model": "stub", "temperature": 0.0,
                "max_tokens": 4096, "timeout_ms": 30000}
        return {"enabled": True,
                "rules": {"enabled": False},
                "models": {"reason": dict(tier), "review": dict(tier)},
                "auto_apply": {"fix": False, "fixAll": False}}

    # -- writing -----------------------------------------------------------

    def _write(self, message):
        with self._wlock:
            try:
                write_message(self._wfile, message)
            except (OSError, ValueError) as exc:
                raise TransportError("write failed: %s" % exc)

    def notify(self, method, params=None):
        self._write(_rpc_message(method, params))

    def send_request(self, method, params=None):
        """Send a request and return `(id, entry)` without waiting for its answer.

        The ordinary path is `request`. This exists for the one step that needs a request to be
        in flight while it does something else: step 10 cancels `jev.explain` mid-model-call,
        and `$/cancelRequest` for a request that has already been answered cancels nothing.
        """
        if self._fatal is not None:
            raise TransportError("transport is dead: %s" % self._fatal)
        if isinstance(params, dict) and params.get("workDoneToken") is not None:
            self._owned_tokens.add(_token_key(params["workDoneToken"]))
        entry = {"method": method, "event": threading.Event(), "done": False,
                 "result": None, "error": None, "t": None}
        with self._cv:
            self._next_id += 1
            rid = self._next_id
            self._pending[rid] = entry
        try:
            self._write(_rpc_message(method, params, rid))
        except Exception:
            with self._cv:
                self._pending.pop(rid, None)
            raise
        return rid, entry

    def await_response(self, rid, entry, budget):
        """Wait for a request sent with `send_request`; return `(result, error)`."""
        deadline = time.monotonic() + budget
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                with self._cv:
                    self._pending.pop(rid, None)
                raise RequestTimeout("no response to %s (id %d) within %.2fs"
                                     % (entry["method"], rid, budget))
            if entry["event"].wait(min(remaining, 0.25)):
                break
            if self._fatal is not None and not entry["done"]:
                with self._cv:
                    self._pending.pop(rid, None)
                raise TransportError("transport died waiting for %s: %s"
                                     % (entry["method"], self._fatal))
        return entry["result"], entry["error"]

    def request(self, method, params=None, timeout=None):
        """Send a request; return its result. Raises ServerError / RequestTimeout /
        TransportError. Responses are matched by id, never by arrival order."""
        rid, entry = self.send_request(method, params)
        result, error = self.await_response(
            rid, entry, self.timeout if timeout is None else timeout)
        if error is not None:
            raise error
        return result

    # -- observation -------------------------------------------------------

    def notifications(self, method=None):
        with self._cv:
            return [dict(m) for m in self._messages
                    if method is None or m["method"] == method]

    def last_response_time(self):
        """Arrival time of the most recent response, for §3.5's token validity window."""
        return self._last_response_t

    def progress(self, token=None):
        out = []
        for message in self.notifications("$/progress"):
            params = message["params"] or {}
            if token is None or _token_key(params.get("token")) == _token_key(token):
                out.append({"value": params.get("value") or {}, "t": message["t"],
                            "token": params.get("token")})
        return out

    def progress_kinds(self, token):
        return [item["value"].get("kind") for item in self.progress(token)]

    def unowned_progress(self):
        out = []
        for item in self.progress():
            if _token_key(item["token"]) not in self._owned_tokens:
                out.append(item["token"])
        return out

    def wait_for(self, predicate, timeout, what):
        """Bounded wait on recorded state. Returns False on timeout or dead transport."""
        deadline = time.monotonic() + timeout
        while True:
            with self._cv:
                if predicate():
                    return True
                if self._fatal is not None:
                    return False
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    log("timed out waiting for %s" % what)
                    return False
                self._cv.wait(min(remaining, 0.25))

    def wait_settle(self, seconds=SETTLE_NEGATIVE_S):
        """Window for a negative assertion: nothing more can arrive afterwards."""
        time.sleep(seconds)

    def changed_uris(self):
        with self._doclock:
            return set(self._changed)

    def _mark_changed(self, uri):
        self._change_log.append((time.monotonic(), uri))
        self._changed.add(uri)

    def unsolicited_publishes(self):
        """`textDocument/publishDiagnostics` that arrived before any change to that
        document — §9 reserves publish for changes the server made."""
        with self._cv:
            publishes = [dict(m) for m in self._messages
                         if m["method"] == "textDocument/publishDiagnostics"]
        with self._doclock:
            changes = list(self._change_log)
        out = []
        for publish in publishes:
            uri = (publish["params"] or {}).get("uri")
            if not any(uri == changed_uri and at < publish["t"] for at, changed_uri in changes):
                out.append(uri)
        return out

    def server_request_count(self, method=None):
        with self._cv:
            return sum(1 for r in self._server_requests
                       if method is None or r["method"] == method)

    def stray_response_count(self):
        with self._cv:
            return len(self._stray_responses)

    def server_requests(self):
        with self._cv:
            return [dict(r) for r in self._server_requests]

    # -- documents ---------------------------------------------------------

    def did_open(self, path, language_id):
        text = pathlib.Path(path).read_text(encoding="utf-8")
        uri = pathlib.Path(path).resolve().as_uri()
        with self._doclock:
            self._docs[uri] = {"version": 1, "text": text, "path": str(path),
                               "language_id": language_id}
        self.notify("textDocument/didOpen", {"textDocument": {
            "uri": uri, "languageId": language_id, "version": 1, "text": text}})
        return uri

    def doc_text(self, uri):
        with self._doclock:
            return self._docs[uri]["text"]

    def doc_version(self, uri):
        with self._doclock:
            return self._docs[uri]["version"]

    def did_change(self, uri, new_text):
        """Replace a document wholesale and sync it, as an editor does."""
        with self._doclock:
            doc = self._docs[uri]
            doc["text"] = new_text
            doc["version"] += 1
            self._mark_changed(uri)
            version = doc["version"]
        self.notify("textDocument/didChange", {
            "textDocument": {"uri": uri, "version": version},
            "contentChanges": [{"text": new_text}]})

    def did_save(self, uri):
        """Save the document. The text travels only if the server asked for it
        (`textDocumentSync.save.includeText`), and ambient analysis is triggered by the
        save (PROTOCOL §10 `triggers.diagnostics`, default `save`)."""
        params = {"textDocument": {"uri": uri}}
        if self.save_include_text:
            with self._doclock:
                params["text"] = self._docs[uri]["text"]
        self.notify("textDocument/didSave", params)

    def resync(self, uri):
        """Sync the text this client currently holds (after applying a server edit)."""
        with self._doclock:
            doc = self._docs[uri]
            version, text = doc["version"], doc["text"]
            self._mark_changed(uri)
        self.notify("textDocument/didChange", {
            "textDocument": {"uri": uri, "version": version},
            "contentChanges": [{"text": text}]})

    # -- the edit contract (§8) --------------------------------------------

    def apply_workspace_edit(self, edit):
        """Apply a WorkspaceEdit to the documents this client owns.

        Returns the LSP ApplyWorkspaceEditResult — `{"applied": bool, "failureReason": str}`
        — which is the answer a client gives to a server's `workspace/applyEdit` (§3.4).
        Refusal, rather than a wrong application, is the correct behaviour for anything §8
        forbids: a bare `changes` map, an absent or non-integer `stale` version, an
        unknown document, overlapping edits.
        """
        try:
            self._apply_workspace_edit(edit)
        except EditRefused as exc:
            return {"applied": False, "failureReason": str(exc)}
        return {"applied": True}

    def _apply_workspace_edit(self, edit):
        if not isinstance(edit, dict):
            raise EditRefused("WorkspaceEdit was not an object")
        if edit.get("changes") is not None:
            raise EditRefused("bare `changes` map is rejected (§8 rule 1)")
        changes = edit.get("documentChanges")
        if changes is None:
            raise EditRefused("documentChanges is absent (§8 rule 1)")
        if not isinstance(changes, list):
            raise EditRefused("documentChanges was not an array")

        staged = {}
        resources = []
        with self._doclock:
            for change in changes:
                if not isinstance(change, dict):
                    raise EditRefused("documentChanges entry was not an object")
                if change.get("kind") is not None:
                    self._validate_resource(change)
                    resources.append(change)
                    continue
                td = change.get("textDocument")
                if not isinstance(td, dict) or "uri" not in td:
                    raise EditRefused("TextDocumentEdit without textDocument.uri")
                uri = td["uri"]
                doc = self._docs.get(uri)
                if doc is None:
                    raise EditRefused("edit targets %s, which this client never synced" % uri)
                version = td.get("version")
                if isinstance(version, bool) or not isinstance(version, int):
                    raise EditRefused(
                        "textDocument.version is %r, not an integer (§8 rule 2)" % (version,))
                if version != doc["version"]:
                    raise EditRefused("refused a stale edit: stamped version %d, document is "
                                      "at %d (§8 rule 3)" % (version, doc["version"]))
                edits = change.get("edits")
                if not isinstance(edits, list) or not edits:
                    raise EditRefused("TextDocumentEdit carries no edits")
                spans = staged.setdefault(uri, [])
                for item in edits:
                    if not isinstance(item, dict):
                        raise EditRefused("edit entry was not an object")
                    new_text = item.get("newText")
                    if not isinstance(new_text, str):
                        raise EditRefused("edit has no string newText")
                    rng = item.get("range")
                    if not isinstance(rng, dict):
                        raise EditRefused("edit has no range")
                    start = self._position_to_offset(doc["text"], rng.get("start"))
                    end = self._position_to_offset(doc["text"], rng.get("end"))
                    if end < start:
                        raise EditRefused("range end precedes its start")
                    spans.append((start, end, new_text))
                spans.sort(key=lambda span: span[0])
                for left, right in zip(spans, spans[1:]):
                    if right[0] < left[1]:
                        raise EditRefused("overlapping edits in one TextDocumentEdit "
                                          "(§8 rule 4)")

            # All-or-nothing: validate everything, then commit.
            for uri, spans in staged.items():
                text = self._docs[uri]["text"]
                for start, end, new_text in sorted(spans, key=lambda s: s[0], reverse=True):
                    text = text[:start] + new_text + text[end:]
                self._docs[uri]["text"] = text
                self._docs[uri]["version"] += 1
                self._mark_changed(uri)
            for operation in resources:
                self._apply_resource(operation)

    def _validate_resource(self, operation):
        kind = operation.get("kind")
        if kind not in ("create", "rename", "delete"):
            raise EditRefused("unknown resource operation %r" % (kind,))
        if "uri" not in operation:
            raise EditRefused("%s operation without a uri" % kind)
        if kind == "rename" and "newUri" not in operation:
            raise EditRefused("rename operation without a newUri")

    def _apply_resource(self, operation):
        kind = operation["kind"]
        url = _uri_to_path(operation["uri"])
        options = operation.get("options") or {}
        if kind == "create":
            if os.path.exists(url):
                if options.get("ignoreIfExists"):
                    return
                if not options.get("overwrite"):
                    raise EditRefused("create: %s already exists" % url)
            directory = os.path.dirname(url)
            if directory:
                os.makedirs(directory, exist_ok=True)
            pathlib.Path(url).write_text("", encoding="utf-8")
        elif kind == "rename":
            target = _uri_to_path(operation["newUri"])
            if not os.path.exists(url):
                if options.get("ignoreIfNotExists"):
                    return
                raise EditRefused("rename: %s does not exist" % url)
            directory = os.path.dirname(target)
            if directory:
                os.makedirs(directory, exist_ok=True)
            os.replace(url, target)
        else:
            if not os.path.exists(url):
                if options.get("ignoreIfNotExists"):
                    return
                raise EditRefused("delete: %s does not exist" % url)
            os.remove(url)
        with self._doclock:
            self._mark_changed(operation["uri"])
            if operation.get("newUri"):
                self._mark_changed(operation["newUri"])

    @staticmethod
    def _position_to_offset(text, position):
        """LSP position -> str offset. positionEncoding is utf-8 (N1), so `character` is a
        byte offset into the line; a character landing mid-codepoint is refused."""
        if not isinstance(position, dict):
            raise EditRefused("range position was not an object")
        line = position.get("line")
        character = position.get("character")
        for value in (line, character):
            if isinstance(value, bool) or not isinstance(value, int):
                raise EditRefused("range position must carry integer line/character")
        starts = _line_starts(text)
        if line < 0 or line >= len(starts):
            raise EditRefused("line %d is outside the document" % line)
        start = starts[line]
        end = text.find("\n", start)
        if end == -1:
            end = len(text)
        raw = text[start:end].encode("utf-8")
        if character < 0 or character > len(raw):
            raise EditRefused("character %d is past the end of line %d" % (character, line))
        try:
            prefix = raw[:character].decode("utf-8")
        except UnicodeDecodeError:
            raise EditRefused("character %d is not a UTF-8 boundary" % character)
        return start + len(prefix)

    # -- teardown ----------------------------------------------------------

    def shutdown(self, timeout=2.0):
        """Ask for a clean exit; never fatal, but logged so a hung server is visible."""
        try:
            self.request("shutdown", None, timeout=timeout)
        except Exception as exc:                       # noqa: BLE001
            log("shutdown request failed: %s" % exc)
        try:
            self.notify("exit", None)
        except Exception as exc:                       # noqa: BLE001
            log("exit notification failed: %s" % exc)

    def reader_alive(self):
        return self._thread.is_alive()

    def close(self):
        """Tear down without ever blocking on a stream another thread is reading.

        A thread parked in `readline` holds the buffered reader's lock, so closing that
        stream from here would deadlock. Order: close our write end (the peer sees EOF),
        let the peer exit, join the reader, and only then close the read handle — killing a
        subprocess first if it outlives the `exit` notification.
        """
        try:
            self._wfile.close()
        except Exception:                              # noqa: BLE001
            pass
        self._thread.join(timeout=2.0)
        if self._process is not None:
            try:
                self._process.wait(timeout=3.0)
            except subprocess.TimeoutExpired:
                log("%s did not exit after `exit`; terminating" % self.name)
                self._process.terminate()
                try:
                    self._process.wait(timeout=3.0)
                except subprocess.TimeoutExpired:
                    self._process.kill()
                    self._process.wait(timeout=3.0)
            self._thread.join(timeout=2.0)
        if self._thread.is_alive():
            log("%s: reader thread is still parked on the stream; leaving that handle open"
                % self.name)
        else:
            try:
                self._rfile.close()
            except Exception:                          # noqa: BLE001
                pass
        if self.stub is not None:
            self.stub.join(timeout=2.0)


# --------------------------------------------------------------------------- report


class Report(object):
    def __init__(self, title, quiet=False):
        self.title = title
        self.quiet = quiet          # counts every assertion, prints only what needs eyes
        self.counts = {"ok": 0, "FAIL": 0, "skip": 0, "warn": 0, "info": 0}
        self.failed_steps = []      # for the defect-injection phase of --selftest

    def _emit(self, status, step, label, detail=""):
        self.counts[status] += 1
        if status == "FAIL":
            self.failed_steps.append(str(step))
        if self.quiet and status in ("ok", "info"):
            return
        prefix = {"ok": "[ok  ]", "FAIL": "[FAIL]", "skip": "[skip]", "warn": "[warn]",
                  "info": "[info]"}[status]
        line = "%s %-4s %s" % (prefix, step, label)
        if detail:
            line += " — %s" % detail
        print(line, flush=True)

    def step(self, number, title):
        if self.quiet:
            return
        print("\n-- step %s: %s" % (number, title), flush=True)

    def ok(self, step, label, detail=""):
        self._emit("ok", step, label, detail)

    def fail(self, step, label, detail=""):
        self._emit("FAIL", step, label, detail)

    def check(self, step, label, condition, detail=""):
        if condition:
            self.ok(step, label, detail)
        else:
            self.fail(step, label, detail)
        return bool(condition)

    def skip(self, step, label, reason):
        self._emit("skip", step, label, reason)

    def warn(self, step, label, detail=""):
        self._emit("warn", step, label, detail)

    def info(self, step, detail):
        self._emit("info", step, detail)

    @property
    def failures(self):
        return self.counts["FAIL"]

    def summary(self):
        counts = self.counts
        print("\n== %s: %d ok, %d FAIL, %d skip, %d warn" %
              (self.title, counts["ok"], counts["FAIL"], counts["skip"], counts["warn"]),
              flush=True)
        return counts["FAIL"]


# --------------------------------------------------------------------------- helpers


def spec_commands():
    """PROTOCOL §6's command set, read from the contract itself: `(allowed, demanded)`.

    Read rather than encoded here, and that is the point of the fix that produced this function:
    the file's claim is that it asserts `PROTOCOL.md`, and a list written into it drifts the
    moment §6 gains a row — which is how eight of the fifteen commands came to be advertised with
    nothing pinning them. A row is `| `jev.name` | arguments | returns | yes|no |`; the section
    ends at `### 6.1`.

    `allowed` is every command §6 names; `demanded` is the ones it marks served. A parse that
    finds nothing is a harness error, not a pass: a check that cannot read the contract must
    never look like a check that read it and found nothing wrong.
    """
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), os.pardir, "PROTOCOL.md")
    try:
        text = pathlib.Path(path).read_text(encoding="utf-8")
    except OSError as exc:
        raise HarnessError("cannot read the contract at %s: %s" % (path, exc))
    sections = text.split("\n## 6. ", 1)
    if len(sections) != 2:
        raise HarnessError("PROTOCOL.md has no `## 6.` heading to read the command set from")
    body = sections[1].split("\n### 6.1", 1)[0]
    allowed, demanded = [], []
    for line in body.splitlines():
        if not line.startswith("|"):
            continue
        cells = [cell.strip() for cell in line.strip("|").split("|")]
        if len(cells) < 2 or not (cells[0].startswith("`jev.") and cells[0].endswith("`")):
            continue
        name = cells[0].strip("`")
        allowed.append(name)
        if cells[-1].lower() == "yes":
            demanded.append(name)
    if len(allowed) < 10 or not demanded:
        raise HarnessError(
            "§6's command table yielded %d command(s) (%d served): %s"
            % (len(allowed), len(demanded), allowed))
    return allowed, demanded


def run_command_coverage(session, report, commands, uri, line, timeout):
    """Step 11 (§2, §6): every advertised command is served.

    Reading the capability block is not enough: a name can be advertised while the handler for it
    has drifted, and the client then gets `not_implemented` from a command the server itself
    promised — which happened here once, when `jev.review` was missing from the server's command
    table while the plugin kept sending it, so every press answered "not implemented".

    Each advertised command is called on a document where it can answer *at all*, and the
    assertion is deliberately narrow: anything other than `not_implemented` proves the arm exists.
    What each command does with these arguments is each command's own business — §6.1's codes and
    step 9's `bad_arguments` check cover the shapes.
    """
    report.step(11, "every advertised command is served (§2, §6)")
    unserved, answered = [], {}
    for command in commands:
        result, _ = _try_request(
            session, report, 11, "workspace/executeCommand",
            {"command": command, "arguments": [{"uri": uri, "line": line}]}, timeout)
        envelope = result if isinstance(result, dict) else {}
        body = envelope.get("error") if isinstance(envelope.get("error"), dict) else {}
        code = body.get("code")
        answered[command] = code or "ok"
        if code == "not_implemented":
            unserved.append(command)
    report.check(11, "every advertised command answers something other than not_implemented "
                     "(%d called)" % len(commands),
                 not unserved,
                 "advertised, and answered not_implemented: %s" % _describe(unserved))
    report.info(11, "answers: %s" % _describe(answered)[:400])


def _cap(server_capabilities, *path):
    node = server_capabilities
    for key in path:
        if not isinstance(node, dict) or key not in node:
            return None
        node = node[key]
    return node


def _describe(value):
    try:
        return json.dumps(value)[:200]
    except (TypeError, ValueError):
        return repr(value)[:200]


def _normalize_actions(result, report, step):
    if result is None:
        return []
    if not isinstance(result, list):
        report.fail(step, "the codeAction result is an array or null",
                    "got %s: %s" % (type(result).__name__, _describe(result)))
        return []
    if any(not isinstance(item, dict) for item in result):
        report.fail(step, "every codeAction entry is an object",
                    "entries: %s" % _describe(result))
        return [item for item in result if isinstance(item, dict)]
    return result


def _try_request(session, report, step, method, params, timeout, label=None):
    """Send a request; on a JSON-RPC error, fail the step and continue.

    A timeout is a step failure too — "never hangs" means the per-request budget turns
    into a FAIL — but three of them mean the server has stopped answering, and the rest of
    the run would only burn the budget, so that is raised as a harness error.
    """
    try:
        result = session.request(method, params, timeout=timeout)
    except ServerError as exc:
        report.fail(step, label or ("%s does not return a JSON-RPC error" % method),
                    _describe(exc.error))
        return None, exc
    except RequestTimeout as exc:
        report.fail(step, label or "%s responds inside the per-request budget" % method,
                    str(exc))
        session.timeout_streak += 1
        if session.timeout_streak >= 3:
            raise HarnessError("the server stopped responding: %s" % exc)
        return None, exc
    session.timeout_streak = 0
    return result, None


def _capability_keys(node):
    """Every key in a capability block, at any depth."""
    if isinstance(node, dict):
        for key, value in node.items():
            yield key
            for nested in _capability_keys(value):
                yield nested
    elif isinstance(node, list):
        for item in node:
            for nested in _capability_keys(item):
                yield nested


# The members LSP 3.17 defines for each capability, and for the sub-objects inside them.
#
# A top-level allowlist is not enough for N6: a non-standard member smuggled *inside* a
# capability that is itself standard — `codeActionProvider.customFlag`, an unknown field in
# `diagnosticProvider` — is exactly as much of an invented surface as a `jev/…` method, and a
# top-level check cannot see it. Every key, at every depth, is therefore checked against the
# specification's vocabulary for the block it sits in.
#
# `None` means "a scalar: nothing may live inside it". A dict value under a `None` shape is
# itself non-standard, and its keys are reported one by one.
CAPABILITY_SHAPES = {
    "positionEncoding": None,
    "textDocumentSync": {
        "openClose": None,
        "change": None,
        "willSave": None,
        "willSaveWaitUntil": None,
        "save": {"includeText": None},
    },
    "codeActionProvider": {
        "resolveProvider": None,
        "codeActionKinds": None,
        "workDoneProgress": None,
    },
    "codeLensProvider": {"resolveProvider": None, "workDoneProgress": None},
    "inlayHintProvider": {"resolveProvider": None, "workDoneProgress": None},
    "hoverProvider": {"workDoneProgress": None},
    "diagnosticProvider": {
        "identifier": None,
        "interFileDependencies": None,
        "workspaceDiagnostics": None,
        "workDoneProgress": None,
    },
    "executeCommandProvider": {"commands": None, "workDoneProgress": None},
}


def _nonstandard_members(node, shape, path=""):
    """Every key of `node`, at any depth, that `shape` does not define."""
    if not isinstance(node, dict):
        return []
    offending = []
    for key, value in node.items():
        if key not in shape:
            offending.append("%s%s" % (path, key))
            continue
        inner = shape[key] if isinstance(shape[key], dict) else {}
        if isinstance(value, dict):
            offending.extend(_nonstandard_members(value, inner, "%s%s." % (path, key)))
        elif isinstance(value, list):
            for index, item in enumerate(value):
                offending.extend(_nonstandard_members(
                    item, inner, "%s%s[%d]." % (path, key, index)))
    return offending


def _server_log_lines(session, limit=5):
    """`window/logMessage` / `window/showMessage` text: what the server says about why it
    did or did not do something, so a red or empty step is diagnosable from the report."""
    out = []
    for method in ("window/logMessage", "window/showMessage"):
        for message in session.notifications(method):
            text = (message["params"] or {}).get("message")
            if text:
                out.append(str(text)[:120])
            if len(out) >= limit:
                return out
    return out


def _pull_diagnostics(session, report, uri, grace):
    """Pull findings, re-pulling briefly.

    PROTOCOL §9 serves findings by pull and refreshes by `workspace/diagnostic/refresh`
    (§3.4), which the server sends when an ambient pass finishes. Step 8 calls this once
    per refresh, with a short grace so a pass that cached microseconds earlier is not
    missed; the loop in step 8 is what handles the superseded-refresh ordering.
    """
    params = {"textDocument": {"uri": uri}, "identifier": "jev"}
    result, _ = _try_request(session, report, 8, "textDocument/diagnostic", params,
                             max(grace, 1.0))
    deadline = time.monotonic() + min(grace, FINDINGS_GRACE_S)
    while (isinstance(result, dict) and not (result.get("items") or [])
           and time.monotonic() < deadline):
        time.sleep(0.2)
        result, _ = _try_request(session, report, 8, "textDocument/diagnostic", params,
                                 max(grace, 1.0))
    return result


def _code_action_params(uri, line, trigger_kind):
    return {
        "textDocument": {"uri": uri},
        "range": {"start": {"line": line, "character": 4},
                  "end": {"line": line, "character": 4}},
        "context": {"diagnostics": [], "triggerKind": trigger_kind},
    }


def _line_of(text, marker):
    for index, line in enumerate(text.split("\n")):
        if marker in line:
            return index
    raise HarnessError("fixture marker %r not found" % marker)


def write_fixtures(directory):
    py_path = os.path.join(directory, "attention.py")
    txt_path = os.path.join(directory, "notes.txt")
    pathlib.Path(py_path).write_text(FIXTURE_PY, encoding="utf-8", newline="\n")
    pathlib.Path(txt_path).write_text(FIXTURE_TXT, encoding="utf-8", newline="\n")
    return py_path, txt_path


# ------------------------------------------------------------- step 10: the other §3.5 paths
#
# §3.5's second rule — "exactly one `end` is sent on every path the server survives" — names
# three paths that reach a command's end without an answer, and one that never reaches it at
# all. Step 9 drives the path that *does* have an answer. These three cannot be driven through
# that session, because each needs a configuration the others must not have: a chat tier
# pointing at nothing, a budget of zero calls a minute, and a model that stalls long enough to
# be cancelled. Each therefore gets a server of its own, in its own temp workspace, and all
# three are asked the same questions: exactly one `begin`, exactly one `end`, and a server that
# is still serving afterwards.
#
# The panic path is pinned by the unit test
# `server::progress_tests::a_panic_between_begin_and_end_still_sends_its_end` in
# `crates/jev-lsp/src/server.rs`, not here: the only way to panic a command over the wire is a
# command the product does not have, and a test-only command behind an environment variable is
# a production surface — a shipped way to crash a command.

PATH_TOKEN = "jev:verify-path-1"
PATH_FIXTURE = """\
def load(path):
    return open(path)
"""
# Nothing is listening on port 9 here. The chat tier has to be pointed somewhere, and the one
# place it must not be pointed is at something that answers: the failure under test is the
# model call's, not an answer's.
PATH_DEAD_ENDPOINT = "http://127.0.0.1:9/v1"
PATH_STUB_DELAY_MS = 4000     # the stall a cancellation has to land inside
PATH_CANCEL_END_S = 2.0       # far under that stall: an `end` arriving inside this window
                              # cannot be the model call finishing, so it can only be the
                              # cancellation closing the token


def _free_port():
    """Bind :0 and keep what the kernel gives, so this harness cannot collide with the suite's
    own stub on 8099 or with a sibling run."""
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()
    return port


def _start_stalled_stub():
    """A `verify/stub_model.py` of our own that stalls every answer, for the cancellation path.

    Not the suite's stub: a four-second stall in the process every other harness shares would
    slow all of them down for the sake of one step.
    """
    here = os.path.dirname(os.path.abspath(__file__))
    port = _free_port()
    process = subprocess.Popen(
        [sys.executable, os.path.join(here, "stub_model.py")],
        env=dict(os.environ, STUB_PORT=str(port), STUB_DELAY_MS=str(PATH_STUB_DELAY_MS)),
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    deadline = time.monotonic() + 10.0
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise HarnessError("the stalled stub exited before answering /health")
        try:
            urllib.request.urlopen("http://127.0.0.1:%d/health" % port, timeout=0.5).close()
            return process, port
        except Exception:                          # noqa: BLE001 — not up yet
            time.sleep(0.05)
    process.kill()
    raise HarnessError("the stalled stub never became ready on port %d" % port)


def _dead_tier_env():
    """Every tier pointed at a port nothing answers on.

    The environment override is applied after the client's settings are merged (PROTOCOL §10,
    `AppState::merge_config`), so this wins over whatever the suite configured for it.
    """
    return {"JEV_BASE_URL": PATH_DEAD_ENDPOINT, "JEV_MODEL": "path-model",
            "JEV_REVIEW_MODEL": "path-model",
            "JEV_DECIDE_BASE_URL": PATH_DEAD_ENDPOINT, "JEV_DECIDE_MODEL": "path-model"}


def _stub_tier_env(port):
    """Every tier pointed at the stalled stub this step started."""
    url = "http://127.0.0.1:%d/v1" % port
    return {"JEV_BASE_URL": url, "JEV_MODEL": "stub-model", "JEV_REVIEW_MODEL": "stub-model",
            "JEV_DECIDE_BASE_URL": url, "JEV_DECIDE_MODEL": "stub-model"}


def _path_settings(**extra):
    """What a step-10 session answers `workspace/configuration` with: `rules.enabled` off, so
    the ambient pass is the chat review, plus whatever the path under test needs."""
    settings = {"enabled": True, "rules": {"enabled": False}}
    settings.update(extra)
    return settings


def _value_detail(value):
    """A response, an envelope or an exception, as text for a detail line."""
    return _describe(str(value) if isinstance(value, Exception) else value)[:170]


def _envelope_of(out):
    """The `{ok, error}` envelope of a command response, as `(envelope, error_body)`."""
    envelope = out["result"] if isinstance(out["result"], dict) else {}
    body = envelope.get("error") if isinstance(envelope.get("error"), dict) else {}
    return envelope, body


def _drive_path(server, timeout, env_extra, settings, prefix, cancel=False):
    """Run one §3.5 failure path against a server of its own; return everything it produced.

    `jev.explain` is the command: it is the one that streams under the token the client
    supplied and reaches a model call, so each of these paths has a `begin` behind it and a
    real chance to leave the token open.
    """
    workspace = tempfile.mkdtemp(prefix=prefix)
    os.makedirs(os.path.join(workspace, ".git"), exist_ok=True)
    path = os.path.join(workspace, "path.py")
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(PATH_FIXTURE)
    env = dict(os.environ)
    env.update(env_extra)
    process = subprocess.Popen([server], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.DEVNULL, cwd=workspace, env=env)
    session = Session(process.stdout, process.stdin, timeout=timeout,
                      name=os.path.basename(server), process=process)
    session.settings = settings
    out = {"kinds": [], "result": None, "error": None, "response_at": None, "end_at": None,
           "end_delay": None, "began": None, "cancelled_at": None, "alive": False,
           "followup": None, "log": []}
    try:
        session.request("initialize", initialize_params(workspace), timeout=timeout)
        session.notify("initialized", {})
        uri = session.did_open(path, "python")
        params = {"command": "jev.explain", "arguments": [{"uri": uri, "line": 0}],
                  "workDoneToken": PATH_TOKEN}
        if cancel:
            rid, entry = session.send_request("workspace/executeCommand", params)
            # `begin` is sent before the model call, so waiting for it is what makes the cancel
            # land *inside* a call — an ordering, not a sleep.
            out["began"] = session.wait_for(
                lambda: "begin" in session.progress_kinds(PATH_TOKEN),
                min(timeout, PROGRESS_WAIT_S), "`begin` before the cancellation")
            out["cancelled_at"] = time.monotonic()
            session.notify("$/cancelRequest", {"id": rid})
            out["result"], out["error"] = session.await_response(rid, entry, timeout)
            out["response_at"] = session.last_response_time()
        else:
            try:
                out["result"] = session.request("workspace/executeCommand", params,
                                                timeout=timeout)
            except ServerError as exc:
                out["error"] = exc
            out["response_at"] = session.last_response_time()
        # Bounded: a token the server never closes has to be a FAIL here, not a hang.
        session.wait_for(lambda: "end" in session.progress_kinds(PATH_TOKEN),
                         min(timeout, PROGRESS_WAIT_S), "the token's `end`")
        # Read after the token has gone quiet. §3.5's "nothing further is published under that
        # token" — the cancellation clause — is an absence, and an absence needs a window; the
        # shape read below then covers a `report` that leaked out after the `end`, which the same
        # shape read at the instant the `end` arrived could not (this is what the cancelled
        # command would do if the abort did not stop it).
        session.wait_settle()
        out["kinds"] = session.progress_kinds(PATH_TOKEN)
        out["end_at"] = next((item["t"] for item in reversed(session.progress(PATH_TOKEN))
                              if (item["value"] or {}).get("kind") == "end"), None)
        if out["cancelled_at"] is not None and out["end_at"] is not None:
            out["end_delay"] = out["end_at"] - out["cancelled_at"]
        out["alive"] = process.poll() is None
        if out["alive"]:
            # The server survived the path it just took: a later command is answered.
            try:
                out["followup"] = session.request(
                    "workspace/executeCommand", {"command": "jev.status", "arguments": [{}]},
                    timeout=timeout)
            except (ServerError, RequestTimeout, TransportError) as exc:
                out["followup_error"] = exc
        out["log"] = _server_log_lines(session)
    finally:
        session.shutdown()
        session.close()
        shutil.rmtree(workspace, ignore_errors=True)
    return out


def _check_path_tokens(report, label, out):
    """The §3.5 shape: one `begin`, one `end`, in that order, and nothing under the token after
    the `end`.

    Read after the socket has gone quiet (`_drive_path`), so this covers a leak that came out
    after the `end` as well as the shape at the instant it arrived — and it is the shape, not the
    order of the `end` and the response, that §3.5's token rule gives the server to keep."""
    kinds = out["kinds"]
    report.check(10, "%s: exactly one `begin` and one `end` arrive under the supplied "
                     "workDoneToken, in that order, with nothing after the `end` (§3.5)" % label,
                 kinds.count("begin") == 1 and kinds.count("end") == 1
                 and kinds[0] == "begin" and kinds[-1] == "end",
                 "kinds=%s" % _describe(kinds))


def _check_path_survives(report, label, out):
    """The server is still serving: the path was survived, not merely outlived."""
    report.check(10, "%s: the server survives the path and answers the next command "
                     "(jev.status)" % label,
                 out["alive"] and isinstance(out["followup"], dict)
                 and out["followup"].get("ok") is True,
                 "alive=%s, followup=%s, server log=%s"
                 % (out["alive"],
                    _value_detail(out["followup"] if out["followup"] is not None
                                  else out.get("followup_error")),
                    _describe(out["log"])[:160]))


def _report_path_interleaving(report, label, out):
    """Record the `end`/response interleaving; assert nothing about its order.

    §3.5 makes the token "valid only until the response to that request is sent", and this server
    honours that in its own order — the `end` is handed over before the command body returns — but
    the *wire* order of the server's own two messages is the transport's merge (step 9's note
    carries the measurement), so either interleaving is recorded here and the shape assertions
    carry the §3.5 claim."""
    report.info(10, "%s: end at %s, response at %s — either interleaving is the transport's "
                    "merge of the server's own two messages (§3.5)"
                 % (label, out["end_at"], out["response_at"]))


def _path_model_error(server, timeout, report):
    label = "model error"
    out = _drive_path(server, timeout, env_extra=_dead_tier_env(),
                      settings=_path_settings(), prefix="jev-path-model-")
    _check_path_tokens(report, label, out)
    envelope, body = _envelope_of(out)
    report.check(10, "%s: the answer is a Result envelope — {ok: false, error: {code: "
                     "\"model_error\"}} (§3.5, §6.1)" % label,
                 envelope.get("ok") is False and body.get("code") == "model_error",
                 "response=%s" % _value_detail(out["result"] if out["result"] is not None
                                               else out["error"]))
    _report_path_interleaving(report, label, out)
    _check_path_survives(report, label, out)


def _path_budget(server, timeout, report):
    label = "budget refusal"
    # `max_calls_per_min: 0` refuses every call before one is made (`budget.rs`:
    # `minute.len() >= max_calls_per_min`, and zero satisfies it). Deliberately not
    # `max_tokens_per_session: 0`: that cap is guarded with `> 0`, so zero there means
    # *unlimited* and a harness built on it would pass for the wrong reason.
    out = _drive_path(server, timeout, env_extra=_dead_tier_env(),
                      settings=_path_settings(budget={"max_calls_per_min": 0}),
                      prefix="jev-path-budget-")
    _check_path_tokens(report, label, out)
    envelope, body = _envelope_of(out)
    report.check(10, "%s: the answer is a Result envelope — {ok: false, error: {code: "
                     "\"over_budget\"}} (§3.5, §6.1)" % label,
                 envelope.get("ok") is False and body.get("code") == "over_budget",
                 "response=%s" % _value_detail(out["result"] if out["result"] is not None
                                               else out["error"]))
    _report_path_interleaving(report, label, out)
    _check_path_survives(report, label, out)


def _path_cancellation(server, timeout, report):
    label = "cancellation"
    stub, port = _start_stalled_stub()
    try:
        out = _drive_path(server, timeout, env_extra=_stub_tier_env(port),
                          settings=_path_settings(), prefix="jev-path-cancel-", cancel=True)
    finally:
        stub.terminate()
        try:
            stub.wait(timeout=5)
        except subprocess.TimeoutExpired:
            stub.kill()
    _check_path_tokens(report, label, out)
    report.check(10, "%s: the client sees -32800 Canceled for the request it cancelled "
                     "(§3.5)" % label,
                 isinstance(out["error"], ServerError) and out["error"].code == -32800
                 and out["result"] is None,
                 "result=%s, error=%s" % (_value_detail(out["result"]),
                                          _value_detail(out["error"])))
    # §3.5 names cancellation as a path whose `end` comes from the token guard being dropped by
    # the abort — not from the work finishing. The model is stalled for twice this window, so an
    # `end` inside it cannot be anything else.
    report.check(10, "%s: the `end` is the cancellation's, not the work's — it arrives within "
                     "%.1fs of the cancel, while the model call stalls for %.1fs (§3.5)"
                     % (label, PATH_CANCEL_END_S, PATH_STUB_DELAY_MS / 1000.0),
                 out["end_delay"] is not None and out["end_delay"] <= PATH_CANCEL_END_S,
                 "`begin` seen before the cancel=%s, end %.3fs after it, kinds=%s"
                 % (out["began"], out["end_delay"] if out["end_delay"] is not None else -1.0,
                    _describe(out["kinds"])))
    _report_path_interleaving(report, label, out)
    _check_path_survives(report, label, out)


def run_failure_paths(server, timeout, report):
    """Step 10 (§3.5): the paths that reach a command's end without an answer, and the one that
    does not reach it at all."""
    report.step(10, "the three reachable §3.5 paths — one begin, one end, and a live server")
    if not server:
        report.info(10, "not driven: each of these needs a server configured differently, and "
                        "the in-process stub serves one configuration — the panic path is "
                        "pinned by the unit test in crates/jev-lsp/src/server.rs")
        return
    _path_model_error(server, timeout, report)
    _path_budget(server, timeout, report)
    _path_cancellation(server, timeout, report)


# --------------------------------------------------------------------------- the nine steps


def run_steps(session, workspace, timeout, report, keep_fixtures=False, server=None):
    """docs/VERIFICATION.md §1, steps 1-9, in order, asserted one by one, then step 10 (§3.5)
    in servers of its own.

    `server` is the binary step 10 starts for its three scenarios; with the in-process stub
    (`--selftest`) there is none, and that step reports why instead of guessing.
    """
    fixture_dir = tempfile.mkdtemp(prefix="jev-lsp-verify-", dir=workspace)
    try:
        py_path, txt_path = write_fixtures(fixture_dir)

        # -- step 1 --------------------------------------------------------
        report.step(1, "initialize -> initialized (capabilities, PROTOCOL §2)")
        result, _ = _try_request(session, report, 1, "initialize",
                                 initialize_params(workspace), timeout,
                                 label="initialize succeeds")
        server_capabilities = (result or {}).get("capabilities") or {}
        report.check(1, "positionEncoding == \"utf-8\" (N1)",
                     _cap(server_capabilities, "positionEncoding") == "utf-8",
                     "got %s" % _describe(_cap(server_capabilities, "positionEncoding")))
        report.check(1, "codeActionProvider.resolveProvider == true",
                     _cap(server_capabilities, "codeActionProvider", "resolveProvider") is True,
                     "got %s" % _describe(_cap(server_capabilities, "codeActionProvider",
                                               "resolveProvider")))
        report.check(1, "diagnosticProvider.identifier == \"jev\"",
                     _cap(server_capabilities, "diagnosticProvider", "identifier") == "jev",
                     "got %s" % _describe(_cap(server_capabilities, "diagnosticProvider",
                                               "identifier")))
        report.check(1, "executeCommandProvider.workDoneProgress == true",
                     _cap(server_capabilities, "executeCommandProvider",
                          "workDoneProgress") is True,
                     "got %s" % _describe(_cap(server_capabilities, "executeCommandProvider",
                                               "workDoneProgress")))
        session.notify("initialized", {})
        commands = _cap(server_capabilities, "executeCommandProvider", "commands") or []
        kinds = _cap(server_capabilities, "codeActionProvider", "codeActionKinds") or []
        report.info(1, "executeCommandProvider.commands=%s codeActionKinds=%s"
                    % (_describe(commands), _describe(kinds)))
        report.info(1, "advertised capability members: %s"
                    % _describe(sorted(server_capabilities.keys()))[:200])
        session.save_include_text = _cap(
            server_capabilities, "textDocumentSync", "save", "includeText") is True
        # PROTOCOL §2/§6: advertise exactly what is served. codeLens/inlayHint remain
        # unasserted either way — the info line above prints the whole advertised member set,
        # so a client author can see it.
        #
        # §6's command set, read from the contract rather than listed here: a list in this file
        # drifts the moment §6 gains a row, and it did — eight of the fifteen commands were
        # advertised with nothing pinning them, in a harness whose whole claim is that it is
        # transcribed from `PROTOCOL.md`.
        allowed, demanded = spec_commands()
        extra = [command for command in commands if command not in allowed]
        report.check(1, "every advertised command is a row of §6 (%d read from the contract: %s)"
                     % (len(allowed), ", ".join(allowed)),
                     not extra,
                     "advertised but not named in §6: %s" % _describe(extra))
        missing = [command for command in demanded if command not in commands]
        report.check(1, "every command §6 serves is advertised", not missing,
                     "§6 serves these and the capability block does not list them: %s"
                     % _describe(missing))
        # The other half of "advertise only what is served": a capability nothing answers for
        # is a lie the client acts on, and inline completion was removed from the server
        # entirely (2026-09-19) — so its absence is asserted rather than assumed.
        report.check(1, "no draft capability is advertised (inline completion was removed)",
                     _cap(server_capabilities, "inlineCompletionProvider") is None,
                     "got %s" % _describe(_cap(server_capabilities, "inlineCompletionProvider")))
        # PROTOCOL N6: no custom LSP method. The advertised block is capabilities, not methods,
        # so a method name cannot appear as a key of it — a member is camelCase, a method is
        # `word/word`. Asserted against the exported capability block itself, and then against
        # the wire: a custom method must be answered MethodNotFound.
        nonstandard = sorted(k for k in _capability_keys(server_capabilities) if "/" in k)
        report.check(1, "no custom method is advertised in the capability block (N6)",
                     not nonstandard,
                     "offending capability keys: %s" % _describe(nonstandard))
        # The same claim at every depth: a member the specification does not define, wherever
        # it sits, is an advertised surface a client will act on.
        unknown_members = sorted(_nonstandard_members(server_capabilities, CAPABILITY_SHAPES))
        report.check(1, "every advertised capability member is a standard one (N6)",
                     not unknown_members,
                     "members the specification does not define (path): %s"
                     % _describe(unknown_members))
        # Sent directly rather than through `_try_request`: an error is the *expected* answer
        # here, and a helper that turns one into a step failure would report the contract as a
        # defect.
        try:
            session.request("jev/inspect", {}, timeout=min(timeout, 5.0))
            custom_error = None
        except ServerError as exc:
            custom_error = exc.error
        except RequestTimeout:
            custom_error = None
        report.check(1, "and one is not served either (N6)",
                     isinstance(custom_error, dict) and custom_error.get("code") == -32601,
                     "got %s" % _describe(custom_error))

        # -- step 2 --------------------------------------------------------
        report.step(2, "textDocument/didOpen fixtures in a temp dir under the workspace")
        # PROTOCOL §10: one section, named `jev`, and the server asks for it by name. The
        # first configuration request is the one that decides every default in force, so its
        # parameters are asserted exactly rather than by searching for the string.
        #
        # Bounded wait, not an immediate read. The ask is the `initialized` handler's first act
        # and `initialized` is a *notification*: nothing orders its handler before this client's
        # next messages, and the read below raced it — one run in 100 under load saw no request
        # here at all, while that same run's step 9 lists `workspace/configuration` among the
        # server->client requests it answered, so the server did ask, later. Waiting costs the
        # assertion nothing it should have had: a server that never asks still fails here once
        # the bound elapses (`--selftest`'s `no_configuration` defect is exactly that case), and
        # so does a server that asks for another section or for an item with no section. What it
        # no longer pins is *when* the ask arrives, within the bound — which §10 never promised:
        # the server's own side of that is `await_config`, not the client's read.
        session.wait_for(lambda: any(r.get("method") == "workspace/configuration"
                                     for r in session.server_requests()),
                         min(timeout, PROGRESS_WAIT_S), "the first workspace/configuration")
        asked = [r for r in session.server_requests()
                 if r.get("method") == "workspace/configuration"]
        first_sections = [
            (item or {}).get("section")
            for item in ((asked[0].get("params") or {}).get("items") or [])
        ] if asked else None
        report.check(2, "the first workspace/configuration asks for [\"jev\"] (§10)",
                     first_sections == ["jev"],
                     "asked for %s" % _describe(first_sections))
        py_uri = session.did_open(py_path, "python")
        txt_uri = session.did_open(txt_path, "plaintext")
        report.info(2, "opened %s (python) and %s (plaintext, control document)"
                    % (py_uri, txt_uri))
        target_line = _line_of(FIXTURE_PY, FIXTURE_MARKER)

        # -- step 3 --------------------------------------------------------
        report.step(3, "textDocument/codeAction, Invoked — the fast path (N2)")
        started = time.perf_counter()
        actions, _ = _try_request(session, report, 3, "textDocument/codeAction",
                                  _code_action_params(py_uri, target_line, 1), timeout)
        elapsed_ms = (time.perf_counter() - started) * 1000.0
        actions = _normalize_actions(actions, report, 3)
        report.check(3, "response within the %.0f ms codeAction budget (ARCHITECTURE §4)"
                    % BUDGET_CODE_ACTION_MS, elapsed_ms <= BUDGET_CODE_ACTION_MS,
                    "observed %.2f ms, %d action(s)" % (elapsed_ms, len(actions)))
        offenders = [index for index, action in enumerate(actions) if "edit" in action]
        report.check(3, "no returned action carries an `edit` (N2)", not offenders,
                     "offending action indexes: %s" % offenders)
        states = [(action.get("data") or {}).get("state") for action in actions]
        report.info(3, "actions=%d data.state=%s" % (len(actions), _describe(states)))
        strange = [(index, _describe((action.get("data") or {}).get("state")))
                   for index, action in enumerate(actions)
                   if action.get("data")
                   and (action.get("data") or {}).get("state") not in ACTION_STATES]
        if strange:
            report.warn(3, "data.state outside the §4 state set", str(strange))
        if not actions:
            report.warn(3, "no actions offered on an Invoked trigger",
                        "PROTOCOL §3.1: a cold cache returns one `disabled` action with "
                        "disabled.reason=\"analyzing…\"; steps 5-7 will be reported as skip")

        # -- step 4 --------------------------------------------------------
        report.step(4, "textDocument/codeAction, triggerKind=2 (Automatic) — no placeholder")
        automatic, _ = _try_request(session, report, 4, "textDocument/codeAction",
                                    _code_action_params(py_uri, target_line, 2), timeout)
        automatic = _normalize_actions(automatic, report, 4)
        bad_states = [(index, _describe((action.get("data") or {}).get("state")))
                      for index, action in enumerate(automatic)
                      if (action.get("data") or {}).get("state") != "ready"]
        report.check(4, "every returned action has data.state == \"ready\" (§3.1)",
                     not bad_states, "offenders (index, state): %s" % bad_states)
        report.info(4, "automatic actions=%d" % len(automatic))

        # -- step 5 --------------------------------------------------------
        report.step(5, "codeAction/resolve — a versioned documentChanges edit (§8, N4)")
        edit = None
        if not actions:
            report.skip(5, "resolve returned edit.documentChanges",
                        "step 3 returned no actions to resolve")
        else:
            resolved, _ = _try_request(session, report, 5, "codeAction/resolve", actions[0],
                                       timeout,
                                       label="codeAction/resolve does not return a "
                                             "JSON-RPC error")
            state = _describe(((resolved or {}).get("data") or {}).get("state"))
            edit = (resolved or {}).get("edit") if isinstance(resolved, dict) else None
            report.check(5, "the resolved action carries an `edit`", edit is not None,
                         "data.state=%s, action keys=%s, server log=%s"
                         % (state, _describe(sorted((resolved or {}).keys())),
                            _describe(_server_log_lines(session))[:160]))
            if edit is None:
                report.skip(5, "edit.documentChanges / no bare `changes` / integer version",
                            "no edit was returned (see the FAIL above)")
            else:
                changes = edit.get("documentChanges")
                report.check(5, "edit.documentChanges is a non-empty array (N4)",
                             isinstance(changes, list) and len(changes) > 0,
                             "got %s" % _describe(changes)[:120])
                report.check(5, "edit carries no bare `changes` map (§8 rule 1)",
                             edit.get("changes") is None,
                             "changes=%s" % _describe(edit.get("changes"))[:120])
                document_edits = [c for c in (changes or [])
                                  if isinstance(c, dict) and "textDocument" in c]
                if not document_edits:
                    report.skip(5, "every TextDocumentEdit.textDocument.version is an integer",
                                "edit carried only resource operations")
                else:
                    offenders = []
                    for index, change in enumerate(document_edits):
                        version = (change.get("textDocument") or {}).get("version")
                        if isinstance(version, bool) or not isinstance(version, int):
                            offenders.append((index, _describe(version)))
                    report.check(5, "every TextDocumentEdit.textDocument.version is an integer "
                                    "(§8 rule 2)", not offenders,
                                 "offenders (index, version): %s" % offenders)

        # -- step 6 --------------------------------------------------------
        report.step(6, "workspace/applyEdit — the edit applies (§3.4, N4)")
        if edit is None:
            report.skip(6, "workspace/applyEdit applied == true",
                        "step 5 produced no edit to apply")
        else:
            applied = session.apply_workspace_edit(edit)
            report.check(6, "workspace/applyEdit -> applied == true",
                         applied.get("applied") is True,
                         "failureReason=%s" % _describe(applied.get("failureReason")))
            if applied.get("applied") is True:
                session.resync(py_uri)
                report.info(6, "applied to %s and synced at version %d"
                            % (py_uri, session.doc_version(py_uri)))

        # -- step 7 --------------------------------------------------------
        report.step(7, "staleness — resolve an action after the document moved (§8 rule 3)")
        aged = _normalize_actions(
            _try_request(session, report, 7, "textDocument/codeAction",
                         _code_action_params(py_uri, target_line, 1), timeout)[0],
            report, 7)
        if not aged:
            report.skip(7, "the stale resolve returns no edit",
                        "no action was offered to age")
        else:
            session.did_change(py_uri, "-- user typed this\n" + session.doc_text(py_uri))
            stale, _ = _try_request(session, report, 7, "codeAction/resolve", aged[0],
                                    timeout,
                                    label="the stale resolve is not a JSON-RPC error "
                                          "(§8 rule 3)")
            if stale is not None:
                state = _describe(((stale or {}).get("data") or {}).get("state"))
                report.check(7, "the stale resolve returns no `edit` (§8 rule 3)",
                             "edit" not in stale,
                             "data.state=%s, edit=%s"
                             % (state, _describe(stale.get("edit"))[:120]))

        # -- step 8 --------------------------------------------------------
        report.step(8, "textDocument/diagnostic — findings carry data (§9)")
        session.did_save(py_uri)
        # §9 serves findings by pull and refreshes by request: each ambient pass ends by
        # asking the client to re-pull (§3.4). A pass that was superseded also refreshes
        # (its conclusion is discarded, not its obligation to notify), and that request can
        # arrive *before* the pass for the content we now hold — so re-pull on every new
        # request until the report describes what the client actually has, bounded. A pull
        # that is empty because the pass has not run yet is not a clean document, and a
        # client that pulls once and believes it is exactly how findings get lost.
        deadline = time.monotonic() + min(timeout, FINDINGS_WAIT_S)
        refreshes, diagnostics = 0, {}
        while True:
            seen = session.server_request_count("workspace/diagnostic/refresh")
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not session.wait_for(
                    lambda: session.server_request_count("workspace/diagnostic/refresh") > seen,
                    remaining, "workspace/diagnostic/refresh after didSave"):
                break
            refreshes += session.server_request_count("workspace/diagnostic/refresh") - seen
            diagnostics = _pull_diagnostics(session, report, py_uri, min(timeout, remaining))
            diagnostics = diagnostics if isinstance(diagnostics, dict) else {}
            if diagnostics.get("items"):
                break
        report.check(8, "the server asks to be re-pulled once its background pass lands "
                        "(workspace/diagnostic/refresh, §3.4/§9)", refreshes > 0,
                     "refresh requests seen after didSave: %d, expected at least 1 within "
                     "%.0fs; without it the pull below is empty for content the server has "
                     "not analysed" % (refreshes, min(timeout, FINDINGS_WAIT_S)))
        report.check(8, "kind == \"full\"", diagnostics.get("kind") == "full",
                     "got %s" % _describe(diagnostics.get("kind")))
        result_id = diagnostics.get("resultId")
        report.check(8, "a resultId is present", result_id is not None,
                     "got %s" % _describe(result_id))
        items = diagnostics.get("items")
        if not isinstance(items, list):
            report.fail(8, "items is an array", "got %s" % _describe(items)[:120])
            items = []
        else:
            missing = []
            for index, item in enumerate(items):
                data = (item or {}).get("data") or {}
                if not data.get("finding_id") or not data.get("verb"):
                    missing.append((index, _describe(data)[:120]))
            report.check(8, "every finding carries data.finding_id and data.verb (§9)",
                         not missing, "offenders: %s" % missing)
            strange = [(index, _describe(((item or {}).get("data") or {}).get("verb")))
                       for index, item in enumerate(items)
                       if ((item or {}).get("data") or {}).get("verb") not in VERBS]
            if strange:
                report.warn(8, "data.verb outside the frozen §4.1 verb set", str(strange))
            report.info(8, "findings=%d sources=%s severities=%s"
                        % (len(items),
                           _describe([(item or {}).get("source") for item in items]),
                           _describe([(item or {}).get("severity") for item in items])))
            if not items:
                report.fail(8, "the pull carries findings for the live content (§9)",
                            "no items: the server has no conclusion for the content this "
                            "client holds, so the §9 item assertions above were vacuous; "
                            "server log: %s" % _describe(_server_log_lines(session))[:200])

        # -- step 9 --------------------------------------------------------
        report.step(9, "workspace/executeCommand + workDoneToken — progress (§3.5)")
        commands = _cap(server_capabilities, "executeCommandProvider", "commands") or []
        if "jev.status" in commands:
            command, arguments = "jev.status", [{}]
        elif commands:
            command, arguments = commands[0], [{}]
        else:
            command, arguments = "jev.status", [{}]
            report.warn(9, "executeCommandProvider.commands is empty (PROTOCOL §2/§6)",
                        "sending jev.status anyway")
        started = time.perf_counter()
        response, error = _try_request(
            session, report, 9, "workspace/executeCommand",
            {"command": command, "arguments": arguments, "workDoneToken": PROGRESS_TOKEN},
            timeout,
            label="workspace/executeCommand does not return a JSON-RPC error")
        response_ms = (time.perf_counter() - started) * 1000.0
        response_at = session.last_response_time()
        report.info(9, "command=%s response=%s in %.2f ms"
                    % (command,
                       _describe(error.error) if error else _describe(response)[:120],
                       response_ms))
        session.wait_for(lambda: ("begin" in session.progress_kinds(PROGRESS_TOKEN)
                                  and "end" in session.progress_kinds(PROGRESS_TOKEN)),
                         min(timeout, PROGRESS_WAIT_S),
                         "progress begin/end for %s" % PROGRESS_TOKEN)
        kinds = session.progress_kinds(PROGRESS_TOKEN)
        report.check(9, "a $/progress `begin` arrived for the supplied workDoneToken",
                     kinds.count("begin") >= 1, "kinds=%s" % _describe(kinds))
        report.check(9, "a $/progress `end` arrived for the supplied workDoneToken",
                     kinds.count("end") >= 1, "kinds=%s" % _describe(kinds))
        report.check(9, "one begin, one end, in that order (§3.5)",
                     kinds.count("begin") == 1 and kinds.count("end") == 1
                     and kinds[0] == "begin" and kinds[-1] == "end",
                     "kinds=%s" % _describe(kinds))
        # The `end`/response interleaving is recorded, not asserted. §3.5 gives the token its
        # lifetime — "It is valid only until the response to that request is sent" — and the
        # server honours it in its own order: the `end` is handed to the transport (awaited, from
        # inside the command body) before the body returns, so before the response exists. What
        # that does not do is order the server's own two messages *on the wire*: `tower-lsp`
        # writes notifications and responses through two arms of one `futures::stream::select`
        # (`transport.rs`), which polls them round-robin, so when both are ready at the same poll
        # the winner is whichever side the round-robin happened to reach. Measured on this row
        # against one unchanged binary: green at `end 207.431844719, response 207.431873793`, red
        # at `end 157.797434027, response 157.797412763`. Asserting that order pins the
        # transport's wakeup pattern; what the server controls — one `begin`, one `end`, and
        # nothing under the token after it — is what is asserted instead (see the closure check
        # below, which is read after the settle window).
        end_seen = [item["t"] for item in session.progress(PROGRESS_TOKEN)
                    if (item["value"] or {}).get("kind") == "end"]
        if end_seen and response is not None:
            # Printed only when the request was answered: a timed-out request leaves
            # `last_response_time()` at the *previous* response, and pairing that with this
            # token's `end` would be a number that means nothing.
            report.info(9, "the server's own two messages: end at %s, response at %s — either "
                           "interleaving is the transport's merge (§3.5)"
                        % (end_seen[-1], response_at))
        # §6/§6.1: a command that is not served — §6.1's words are "asserted against a name
        # no version serves" — answers structurally instead of vanishing.
        unserved, _ = _try_request(
            session, report, 9, "workspace/executeCommand",
            {"command": "jev.nonexistent", "arguments": [{}]}, timeout,
            label="an unserved command does not return a JSON-RPC error")
        envelope = unserved if isinstance(unserved, dict) else {}
        error_body = envelope.get("error") if isinstance(envelope.get("error"), dict) else {}
        report.check(9, "an unserved command answers structurally — jev.nonexistent -> "
                        "{ok: false, error: {code: \"not_implemented\"}} (§6.1)",
                     envelope.get("ok") is False
                     and error_body.get("code") == "not_implemented",
                     "response=%s" % _describe(unserved)[:160])
        # §6.1: `bad_arguments` is "arguments are missing or malformed; the message names the
        # shape it needs" — a different code from not_implemented, so the plugin can tell a
        # typo in the arguments from a missing feature.
        bad_args, _ = _try_request(
            session, report, 9, "workspace/executeCommand",
            {"command": "jev.plan", "arguments": []}, timeout,
            label="a served command with unusable arguments does not return a JSON-RPC error")
        envelope = bad_args if isinstance(bad_args, dict) else {}
        error_body = envelope.get("error") if isinstance(envelope.get("error"), dict) else {}
        report.check(9, "a served command with unusable arguments answers bad_arguments, "
                        "not not_implemented — jev.plan with [] (§6.1)",
                     envelope.get("ok") is False
                     and error_body.get("code") == "bad_arguments"
                     and bool(error_body.get("message")),
                     "response=%s" % _describe(bad_args)[:160])
        unowned = session.unowned_progress()
        report.check(9, "no $/progress under a token this client never supplied or created "
                        "(§3.5)", not unowned, "unowned tokens: %s" % _describe(unowned))
        session.wait_settle()
        # §3.5's "a token left open is a defect": the `end` closes the token, and nothing arrives
        # under it afterwards. This is a fresh read after the settle window, not the shape read
        # the instant the `end` arrived, so a leak that came out after that read is caught here
        # and only here — which is what the `progress_after_end` defect in `--selftest` injects.
        token_progress = session.progress(PROGRESS_TOKEN)
        ends = [item["t"] for item in token_progress
                if (item["value"] or {}).get("kind") == "end"]
        after_end = [item["value"].get("kind") for item in token_progress
                     if ends and item["t"] > ends[-1]]
        report.check(9, "nothing arrives under the supplied token after its `end` — the token "
                        "is closed, not paused (§3.5)",
                     bool(ends) and not after_end,
                     "end at %s, kinds after it: %s"
                     % (ends[-1] if ends else None, _describe(after_end)))
        publishes = session.notifications("textDocument/publishDiagnostics")
        unsolicited = session.unsolicited_publishes()
        report.check(9, "no textDocument/publishDiagnostics before the server's own change "
                        "to that document (§9)", not unsolicited,
                     "unsolicited uris: %s" % _describe(unsolicited))
        report.info(9, "publishDiagnostics total=%d (documents this client changed: %s)"
                    % (len(publishes), _describe(sorted(session.changed_uris()))[:160]))
        report.info(9, "server->client requests answered: %s"
                    % _describe([r["method"] for r in session.server_requests()])[:200])
        server_log = _server_log_lines(session)
        if server_log:
            report.info(9, "server log: %s" % _describe(server_log)[:220])
        report.check(9, "no response arrived for a message that carried no id (JSON-RPC "
                        "replies to notifications are invalid)",
                     session.stray_response_count() == 0,
                     "stray responses: %d" % session.stray_response_count())
    finally:
        if keep_fixtures:
            log("fixtures kept at %s" % fixture_dir)
        else:
            shutil.rmtree(fixture_dir, ignore_errors=True)
    run_failure_paths(server, timeout, report)
    run_command_coverage(session, report, commands, py_uri, target_line, timeout)
    return report


def initialize_params(workspace):
    root = pathlib.Path(workspace).resolve()
    return {
        "processId": os.getpid(),
        "clientInfo": {"name": "jev-lsp-verify", "version": "1"},
        "locale": "en",
        "rootUri": root.as_uri(),
        "capabilities": client_capabilities(),
        "initializationOptions": {},
        "trace": "off",
        "workspaceFolders": [{"uri": root.as_uri(), "name": root.name}],
    }


# --------------------------------------------------------------------------- in-process stub


class StubServer(threading.Thread):
    """A contract-faithful LSP server for `--selftest`, in this file, on purpose: the
    client must be proven before the Rust server exists. It is not the product and shares
    no code with `crates/`.

    Modes:
      actions    full nine-step surface, one action offered (steps 5-7 run for real)
      none       the same, offering zero actions (proves steps 5-7 report skip)
      correlate  also holds `initialize` until a second request arrives, then answers the
                 second one first — a client that matched responses by arrival order
                 instead of by id would fail
    """

    # §6's served rows, read from the contract: the stub exists to be contract-faithful, and a
    # second copy of the command set here is exactly the drift this file is supposed to catch.
    SERVED_COMMANDS = tuple(spec_commands()[1])

    def __init__(self, rfile, wfile, mode="actions", defect=None, superseded=False):
        threading.Thread.__init__(self, name="stub-server", daemon=True)
        self._rfile = rfile
        self._wfile = wfile
        self._wlock = threading.Lock()
        self.mode = mode
        self.defect = defect
        # `superseded`: model the ordering the real server produces when a pass is
        # interrupted — refresh #1 arrives for the discarded run (the pull is still empty),
        # refresh #2 arrives once the pass for the live content has cached.
        self.superseded = superseded
        self.current_ready = not superseded
        self.refreshes_sent = 0
        self.docs = {}
        self.action_versions = {}
        self.param_members = {}     # method -> [whether `params` was on the wire]
        self.did_save_params = []
        self.config_answers = []   # answers to the `workspace/configuration` this stub asks for
        self.seen = []
        self.error = None
        self.refresh_sent = 0
        self.refresh_acked = 0
        self.publish_sent = 0
        self.other_responses = []
        self._refresh_ids = set()
        self._awaiting = {}
        self._held_initialize = None
        self._next_server_id = 5000

    # -- wire --------------------------------------------------------------

    def _write(self, payload):
        with self._wlock:                              # the timer thread writes too
            write_message(self._wfile, payload)

    def _respond(self, msg_id, result):
        self._write({"jsonrpc": "2.0", "id": msg_id, "result": result})

    def _error(self, msg_id, code, message):
        self._write({"jsonrpc": "2.0", "id": msg_id,
                     "error": {"code": code, "message": message}})

    def _notify(self, method, params=None):
        self._write(_rpc_message(method, params))

    def _request(self, method, params, kind, origin=None):
        self._next_server_id += 1
        rid = self._next_server_id
        self._awaiting[rid] = {"kind": kind, "origin": origin}
        self._write(_rpc_message(method, params, rid))
        return rid

    def _progress(self, token, kind, **extra):
        value = {"kind": kind}
        value.update(extra)
        self._notify("$/progress", {"token": token, "value": value})

    # -- loop --------------------------------------------------------------

    def run(self):
        try:
            self._loop()
        except _StubExit:
            pass
        except Exception:                              # noqa: BLE001 — surfaced as `error`
            self.error = traceback.format_exc()
        finally:
            # Closing our write end is what lets the client's reader thread see EOF and
            # exit; without it the client would park in readline forever.
            try:
                self._wfile.close()
            except Exception:                          # noqa: BLE001
                pass

    def _send_refresh(self):
        self.refreshes_sent += 1
        rid = self._request("workspace/diagnostic/refresh", None, "refresh")
        self._refresh_ids.add(rid)

    def _become_current(self):
        self.current_ready = True
        self._send_refresh()

    def _flush_held(self):
        """Correlate mode: answer a held `initialize` only once another request has been
        answered first, so a client that matched responses by arrival order would fail."""
        if self._held_initialize is None:
            return
        if not any(method == "stub/echo" for method, _ in self.seen):
            return
        held = self._held_initialize
        self._held_initialize = None
        self._respond(held["id"], self._initialize_result())

    def _loop(self):
        while True:
            message = read_message(self._rfile)
            if message is None:
                return
            if "method" in message:
                # Recorded so --selftest can assert that no-params members are sent with
                # the member absent rather than null (see _rpc_message).
                self.param_members.setdefault(message["method"], []).append(
                    "params" in message)
            if "method" in message and "id" in message:
                self._handle_request(message)
            elif "method" in message:
                self._handle_notification(message)
            elif "id" in message:
                self._handle_response(message)
            else:
                return

    def _handle_response(self, message):
        rid = message["id"]
        if rid in self._refresh_ids:
            self.refresh_acked += 1
            return
        state = self._awaiting.pop(rid, None)
        if state is None:
            self.other_responses.append(message)
            return
        if state["kind"] == "ask":
            self._respond(state["origin"], {"body": message.get("result"),
                                            "error": message.get("error")})
        elif state["kind"] == "config":
            # The answer to the request above, kept so a phase can assert the client gave one.
            self.config_answers.append(message.get("result"))
        elif state["kind"] == "unknown":
            error = message.get("error") or {}
            self._respond(state["origin"], {"error_code": error.get("code")})

    def _handle_request(self, message):
        method = message["method"]
        params = message.get("params") or {}
        msg_id = message["id"]
        self.seen.append((method, params))

        if method == "initialize":
            if self.mode == "correlate":
                self._held_initialize = message
                self._flush_held()
                return
            self._respond(msg_id, self._initialize_result())
        elif method == "textDocument/codeAction":
            if self.defect == "slow_code_action":
                time.sleep(0.12)                       # blows the 50 ms budget
            uri = (params.get("textDocument") or {}).get("uri")
            version = (self.docs.get(uri) or {}).get("version", 1)
            self.action_versions[uri] = version
            if self.mode == "none":
                self._respond(msg_id, [])
            else:
                action = self._action(uri, version)
                if self.defect == "edit_on_fast_path":
                    action["edit"] = {"documentChanges": [{
                        "textDocument": {"uri": uri, "version": version},
                        "edits": []}]}
                self._respond(msg_id, [action])
        elif method == "codeAction/resolve":
            action = self._resolve(message["params"])
            if self.defect == "bare_changes":
                uri = ((action.get("data") or {}).get("doc") or {}).get("uri")
                action.pop("edit", None)
                action["changes"] = {uri: [{"range": {"start": {"line": 0, "character": 0},
                                                      "end": {"line": 0, "character": 0}},
                                            "newText": "# jev: bare changes\n"}]}
            elif self.defect == "missing_version":
                for change in ((action.get("edit") or {}).get("documentChanges") or []):
                    (change.get("textDocument") or {}).pop("version", None)
            self._respond(msg_id, action)
        elif method == "textDocument/diagnostic":
            uri = (params.get("textDocument") or {}).get("uri")
            version = (self.docs.get(uri) or {}).get("version", 0)
            items = [] if not self.current_ready else [{
                "range": {"start": {"line": 5, "character": 4},
                          "end": {"line": 5, "character": 20}},
                "severity": 2,
                "source": "jev",
                "message": "unchecked error path",
                "data": {"finding_id": "stub-finding-1", "verb": "fix",
                         "content_hash": "sha256:stub"},
            }]
            self._respond(msg_id, {
                "kind": "full",
                "resultId": "stub-diag-%d" % version,
                "items": items,
            })
        elif method == "workspace/executeCommand":
            command = params.get("command")
            arguments = params.get("arguments") or []
            token = params.get("workDoneToken")
            if command not in self.SERVED_COMMANDS:
                # §6: not served — an unknown name, or one a future version adds before it
                # is implemented — answers structurally, not silently.
                self._respond(msg_id, {
                    "schema": "jev.result/1", "ok": False, "artifacts": [],
                    "diagnostics": [], "edit_ids": [],
                    "error": {"code": "not_implemented",
                              "message": "%s is not implemented in this version" % command}})
            elif command == "jev.plan" and not any(
                    isinstance(argument, dict) and argument.get("goal")
                    for argument in arguments):
                # §6: served, but these arguments are unusable — a different code, so a
                # caller can tell a typo from a missing feature.
                self._respond(msg_id, {
                    "schema": "jev.result/1", "ok": False, "artifacts": [],
                    "diagnostics": [], "edit_ids": [],
                    "error": {"code": "bad_arguments",
                              "message": "jev.plan needs {goal, scope}"}})
            else:
                if token is None:
                    pass                               # no token supplied: no progress due
                elif self.defect == "token_in_arguments":
                    # The §3.5 defect the frozen contract calls out: a token smuggled
                    # through `arguments` instead of the request's workDoneToken field.
                    smuggled = next((item.get("token") for item in arguments
                                     if isinstance(item, dict) and item.get("token")),
                                    "jev:smuggled")
                    self._progress(smuggled, "begin")
                    self._progress(smuggled, "end")
                elif self.defect == "unowned_progress":
                    self._progress("jev:never-supplied", "begin")
                    self._progress("jev:never-supplied", "end")
                elif self.defect == "no_progress_end":
                    self._progress(token, "begin", title="stub", percentage=0)
                    self._progress(token, "report", message="halfway", percentage=50)
                else:
                    self._progress(token, "begin", title="stub", percentage=0)
                    self._progress(token, "report", message="halfway", percentage=50)
                    self._progress(token, "end", message="done")
                self._respond(msg_id, {"schema": "jev.result/1", "ok": True, "artifacts": [],
                                       "diagnostics": [], "edit_ids": []})
                if self.defect == "progress_after_end" and token is not None:
                    # After the answer, and after the harness has read the token's shape once:
                    # `$/progress` under a token its `end` has already closed. Late on purpose —
                    # an immediate leak would be caught by that first shape read, and this defect
                    # exists to prove the settled one.
                    threading.Timer(
                        0.10,
                        lambda: self._progress(token, "report", message="leaked")).start()
        elif method == "shutdown":
            self._respond(msg_id, None)
        elif method == "stub/echo":
            self._respond(msg_id, params)
            self._flush_held()
        elif method == "stub/boom":
            self._error(msg_id, -32000, "boom")
        elif method == "stub/silent":
            pass                                        # deliberately unanswered
        elif method == "stub/notify":
            self.publish_sent += 1
            self._notify("textDocument/publishDiagnostics",
                         {"uri": params.get("uri"), "diagnostics": []})
            self._respond(msg_id, {"sent": True})
        elif method == "stub/progress":
            token = params.get("workDoneToken")
            self._progress(token, "begin")
            self._progress(token, "report")
            self._progress(token, "end")
            self._respond(msg_id, {"token": token})
        elif method == "stub/ask":
            self._request("workspace/configuration",
                          {"items": [{"section": "jev"}]}, "ask", origin=msg_id)
        elif method == "stub/unknown-request-back":
            self._request("stub/unknownServerRequest", {}, "unknown", origin=msg_id)
        else:
            self._error(msg_id, -32601, "not implemented: %s" % method)

    def _handle_notification(self, message):
        method = message["method"]
        params = message.get("params") or {}
        self.seen.append((method, params))
        if method == "initialized":
            # A faithful server's first act is `JevServer::initialized` -> `pull_configuration`:
            # ask the client for the `jev` section *by name*. Step 2 asserts exactly that of the
            # first configuration request, and it was unsatisfiable here — this stub never asked,
            # so every baseline phase of this selftest reported that assertion red.
            #
            # `no_configuration` is that server again, with the ask removed: step 2 must still
            # fail on it, which is what says the bounded wait in step 2 bought tolerance for a
            # late ask and not tolerance for a server that never asks at all.
            if self.defect != "no_configuration":
                self._request("workspace/configuration", {"items": [{"section": "jev"}]}, "config")
        if method == "textDocument/didOpen":
            document = params["textDocument"]
            self.docs[document["uri"]] = {"version": document.get("version", 1),
                                          "text": document.get("text", "")}
            self.refresh_sent += 1
            rid = self._request("workspace/diagnostic/refresh", None, "refresh")
            self._refresh_ids.add(rid)
            if self.defect == "unsolicited_publish":
                self.publish_sent += 1
                self._notify("textDocument/publishDiagnostics",
                             {"uri": document["uri"], "diagnostics": []})
        elif method == "textDocument/didChange":
            document = params["textDocument"]
            entry = self.docs.setdefault(document["uri"], {"version": 0, "text": ""})
            entry["version"] = document.get("version", entry["version"])
            for change in params.get("contentChanges") or []:
                if "range" not in change:
                    entry["text"] = change.get("text", "")
        elif method == "textDocument/didSave":
            self.did_save_params.append(params)
            self._send_refresh()
            if self.superseded:
                # The interrupted pass's refresh, then the live pass's — the order a real
                # server produces, and the order one-pull-after-one-refresh gets wrong.
                timer = threading.Timer(0.25, self._become_current)
                timer.daemon = True
                timer.start()
            else:
                self.current_ready = True
        elif method == "exit":
            raise _StubExit()

    # -- contract-faithful payloads ---------------------------------------

    def _initialize_result(self):
        """Mirrors PROTOCOL §2 after the 2026-09-18 amendment: advertise only what is
        served, and the served command set is §6's."""
        capabilities = {
            "positionEncoding": "utf-8",
            "textDocumentSync": {"openClose": True, "change": 2,
                                 "save": {"includeText": False}},
            "codeActionProvider": {
                "resolveProvider": True,
                "codeActionKinds": ["quickfix", "quickfix.jev", "refactor.rewrite",
                                    "refactor.rewrite.jev", "source", "source.jev",
                                    "source.fixAll"],
            },
            "diagnosticProvider": {"identifier": "jev", "interFileDependencies": False,
                                   "workspaceDiagnostics": True},
            "executeCommandProvider": {
                "commands": list(self.SERVED_COMMANDS),
                "workDoneProgress": True},
        }
        if self.defect == "extra_capability_member":
            # A member the specification does not define, *inside* a capability that is
            # standard: invisible to a top-level allowlist, and exactly what N6 forbids.
            capabilities["codeActionProvider"]["customFlag"] = True
        return {"capabilities": capabilities,
                "serverInfo": {"name": "verify-stub", "version": "0"}}

    def _action(self, uri, version):
        return {
            "title": "jev: harden parse_retry",
            "kind": "refactor.rewrite.jev",
            "isPreferred": True,
            "data": {"v": 1, "id": "stub-action-1", "verb": "harden", "state": "ready",
                     "doc": {"uri": uri, "version": version, "content_hash": "sha256:stub"},
                     "scope": {"kind": "function", "name": "parse_retry"},
                     "language": "python", "scope_source": "tree"},
        }

    def _resolve(self, action):
        data = action.get("data") or {}
        uri = (data.get("doc") or {}).get("uri")
        current = (self.docs.get(uri) or {}).get("version")
        stamped = self.action_versions.get(uri)
        if (current is None or stamped is None or current != stamped) \
                and self.defect != "ignore_staleness":
            data["state"] = "stale"
            action["data"] = data
            action.pop("edit", None)
            return action
        text = self.docs[uri]["text"]
        lines = text.split("\n")
        last = len(lines) - 1
        action["edit"] = {"documentChanges": [{
            "textDocument": {"uri": uri, "version": stamped},
            "edits": [{
                "range": {"start": {"line": 0, "character": 0},
                          "end": {"line": last,
                                  "character": len(lines[last].encode("utf-8"))}},
                "newText": text + "\n# jev: hardened\n",
            }],
        }]}
        return action


class _StubExit(Exception):
    pass


def _stub_session(mode="actions", timeout=DEFAULT_TIMEOUT, defect=None, superseded=False):
    """Client and in-process stub over two real pipes: real Content-Length framing, no
    subprocess, no shared code with the server under test."""
    client_to_server_r, client_to_server_w = os.pipe()
    server_to_client_r, server_to_client_w = os.pipe()
    stub_read = os.fdopen(client_to_server_r, "rb")
    stub_write = os.fdopen(server_to_client_w, "wb")
    client_read = os.fdopen(server_to_client_r, "rb")
    client_write = os.fdopen(client_to_server_w, "wb")
    stub = StubServer(stub_read, stub_write, mode=mode, defect=defect, superseded=superseded)
    stub.start()
    session = Session(client_read, client_write, timeout=timeout,
                      name="stub(%s%s%s)" % (mode, "" if defect is None else "/" + defect,
                                             "/superseded" if superseded else ""))
    session.stub = stub
    return session, stub, (stub_read, stub_write, client_read, client_write)


def _close_stub_session(session, files):
    """Close what can be closed. A stream whose thread is still parked in `readline`
    cannot be closed from here without deadlocking on the buffered reader's lock, so it is
    left open on purpose; every thread here is a daemon, so the process still exits."""
    session.close()
    stub = session.stub
    stub_alive = bool(stub is not None and stub.is_alive())
    for handle, owner_alive in ((files[0], stub_alive), (files[1], stub_alive),
                                (files[2], session.reader_alive()), (files[3], False)):
        if owner_alive:
            log("not closing %r: its thread is still parked on it" % (handle,))
            continue
        try:
            handle.close()
        except Exception:                              # noqa: BLE001
            pass


# --------------------------------------------------------------------------- selftest


def run_selftest(timeout):
    """Prove the client without the Rust server: framing, correlation, all nine steps, and
    that injected contract defects actually turn the harness red."""
    report = Report("selftest")
    print("== selftest: framing, request/response correlation, the nine steps, and defect "
          "injection, all against an in-process stub")

    # -- phase 1: transport behaviour --------------------------------------
    print("\n-- phase 1: transport (Content-Length framing, id correlation, timeouts, "
          "server requests)")
    session, stub, files = _stub_session(mode="correlate", timeout=timeout)
    try:
        echoed = []
        payload = {"text": "café — 日本語 — ✔", "bytes": "ü" * 40}
        worker = threading.Thread(target=lambda: echoed.append(
            session.request("stub/echo", payload, timeout=timeout)), daemon=True)
        worker.start()
        initialized = session.request("initialize", initialize_params(os.getcwd()),
                                     timeout=timeout)
        worker.join(timeout=timeout)
        report.check("1.1", "initialize answered while a later request was in flight",
                     _cap((initialized or {}).get("capabilities") or {},
                          "positionEncoding") == "utf-8",
                     "capabilities=%s" % _describe((initialized or {}).get("capabilities"))[:80])
        report.check("1.2", "responses correlate by id, not arrival order (the stub answered "
                            "the second request first)",
                     echoed and echoed[0] == payload,
                     "echoed=%s" % _describe(echoed)[:120])

        try:
            session.request("stub/boom", {})
            report.fail("1.4", "a JSON-RPC error surfaces as ServerError", "no error raised")
        except ServerError as exc:
            report.check("1.4", "a JSON-RPC error surfaces as ServerError",
                         exc.code == -32000, "code=%s message=%s"
                         % (exc.code, _describe(exc.error.get("message"))))

        started = time.perf_counter()
        try:
            session.request("stub/silent", {}, timeout=0.4)
            report.fail("1.5", "an unanswered request raises RequestTimeout", "returned")
        except RequestTimeout:
            waited = time.perf_counter() - started
            report.check("1.5", "an unanswered request raises RequestTimeout (never hangs)",
                         waited < 2.0, "waited %.2fs for a 0.4s budget" % waited)

        session.request("stub/notify", {"uri": "file:///tmp/selftest-control.txt"})
        report.check("1.6", "a server notification is recorded in arrival order",
                     session.wait_for(
                         lambda: session.notifications("textDocument/publishDiagnostics"),
                         2.0, "publishDiagnostics"),
                     "recorded=%d" % len(session.notifications(
                         "textDocument/publishDiagnostics")))

        session.request("stub/progress", {"workDoneToken": "jev:selftest-1"})
        kinds = session.progress_kinds("jev:selftest-1")
        report.check("1.7", "progress is recorded under the token the client supplied",
                     kinds == ["begin", "report", "end"], "kinds=%s" % _describe(kinds))

        asked = session.request("stub/ask", {})
        # One entry per requested item, and that entry is the client's `jev` section — not an
        # empty object. `rules.enabled: false` is what every later step's measurements depend on
        # (the ambient pass is then the chat review this client is written against), so an answer
        # that carried fewer keys would quietly change what the rest of the run is measuring.
        answer = asked.get("body") if isinstance(asked, dict) else None
        report.check("1.8", "the client answers a server->client workspace/configuration "
                            "request with its `jev` section",
                     isinstance(answer, list) and len(answer) == 1
                     and isinstance(answer[0], dict)
                     and (answer[0].get("rules") or {}).get("enabled") is False,
                     "server saw %s" % _describe(asked)[:160])

        unknown = session.request("stub/unknown-request-back", {})
        report.check("1.9", "the client replies MethodNotFound to an unknown server->client "
                            "request", isinstance(unknown, dict)
                     and unknown.get("error_code") == -32601,
                     "server saw %s" % _describe(unknown)[:160])

        session.shutdown()
        session.close()
        report.check("1.10", "the stub thread exited without a protocol error",
                     stub.error is None and not stub.is_alive(),
                     "error=%s alive=%s" % (_describe(stub.error)[:200], stub.is_alive()))
        report.check("1.11", "the client answered only the requests the stub made",
                     not stub.other_responses,
                     "unexpected responses: %d" % len(stub.other_responses))
    finally:
        _close_stub_session(session, files)

    # -- phase 2: the nine steps, actions offered --------------------------
    print("\n-- phase 2: the nine steps against the stub, actions offered")
    session, stub, files = _stub_session(mode="actions", timeout=timeout)
    try:
        offline = Report("selftest phase 2")
        run_steps(session, tempfile.gettempdir(), timeout, offline)
        session.shutdown()
        session.close()
        report.check("2.1", "the nine steps pass against a contract-faithful stub",
                     offline.failures == 0,
                     "ok=%d FAIL=%d skip=%d warn=%d"
                     % (offline.counts["ok"], offline.counts["FAIL"],
                        offline.counts["skip"], offline.counts["warn"]))
        report.check("2.2", "steps 5-7 ran instead of skipping (one action was offered)",
                     offline.counts["skip"] == 0, "skips=%d" % offline.counts["skip"])
        report.check("2.3", "the stub thread exited without a protocol error",
                     stub.error is None, "error=%s" % _describe(stub.error)[:200])
        report.check("2.4", "no-params members go out without a `params` member, not with "
                            "null (`shutdown`, `exit`)",
                     stub.param_members.get("shutdown") == [False]
                     and stub.param_members.get("exit") == [False],
                     "params present? shutdown=%s exit=%s"
                     % (stub.param_members.get("shutdown"), stub.param_members.get("exit")))
        report.check("2.5", "didSave carries no `text` when the server advertises "
                            "textDocumentSync.save.includeText = false (§2)",
                     stub.did_save_params
                     and all("text" not in params for params in stub.did_save_params),
                     "saves: %d, keys: %s" % (len(stub.did_save_params),
                                              _describe(stub.did_save_params)[:120]))
    finally:
        _close_stub_session(session, files)

    # -- phase 2b: the superseded-refresh ordering -------------------------
    print("\n-- phase 2b: a superseded pass refreshes before the pass for the live content")
    session, stub, files = _stub_session(mode="actions", timeout=timeout, superseded=True)
    try:
        late = Report("selftest phase 2b", quiet=True)
        run_steps(session, tempfile.gettempdir(), timeout, late)
        session.shutdown()
        session.close()
        report.check("2.6", "the scenario really happened: the stub sent a refresh for the "
                            "discarded run and then a second for the live content",
                     stub.refreshes_sent >= 2, "refreshes sent: %d" % stub.refreshes_sent)
        report.check("2.7", "step 8 re-pulls per refresh and still observes findings for "
                            "the live content (one pull after one refresh would go empty)",
                     late.failures == 0,
                     "ok=%d FAIL=%d failed steps=%s"
                     % (late.counts["ok"], late.counts["FAIL"],
                        sorted(set(late.failed_steps))))
    finally:
        _close_stub_session(session, files)

    # -- phase 3: the nine steps, no actions offered -----------------------
    print("\n-- phase 3: the nine steps against the stub with zero actions (skip handling)")
    session, stub, files = _stub_session(mode="none", timeout=timeout)
    try:
        empty = Report("selftest phase 3")
        run_steps(session, tempfile.gettempdir(), timeout, empty)
        session.shutdown()
        session.close()
        report.check("3.1", "zero actions do not fail the run",
                     empty.failures == 0,
                     "ok=%d FAIL=%d skip=%d warn=%d"
                     % (empty.counts["ok"], empty.counts["FAIL"],
                        empty.counts["skip"], empty.counts["warn"]))
        report.check("3.2", "steps 5, 6 and 7 are reported as skip, never as a pass",
                     empty.counts["skip"] == 3, "skips=%d" % empty.counts["skip"])
        report.check("3.3", "the stub thread exited without a protocol error",
                     stub.error is None, "error=%s" % _describe(stub.error)[:200])
    finally:
        _close_stub_session(session, files)

    # -- phase 4: injected defects ----------------------------------------
    print("\n-- phase 4: defect injection — every injected defect must fail the step that "
          "specifies it (docs/VERIFICATION.md §4)")
    defects = [
        ("edit_on_fast_path", "3", "an action carrying an `edit` on the fast path (N2)"),
        ("no_configuration", "2", "a server that never asks for the `jev` section (§10)"),
        ("slow_code_action", "3", "a codeAction that misses the 50 ms budget"),
        ("bare_changes", "5", "`documentChanges` replaced by a bare `changes` map"),
        ("missing_version", "5", "a TextDocumentEdit without a `version` (§8 rule 2)"),
        ("ignore_staleness", "7", "an edit returned for an action the document moved past"),
        ("no_progress_end", "9", "a `$/progress` token left without an `end`"),
        ("progress_after_end", "9", "a `$/progress` published under a token its `end` closed"),
        ("unowned_progress", "9", "progress under a token the client never supplied"),
        ("token_in_arguments", "9", "a token smuggled through `arguments` instead of "
                                    "workDoneToken"),
        ("unsolicited_publish", "9", "publishDiagnostics for a document the server did "
                                     "not change"),
        ("extra_capability_member", "1", "a non-standard member nested inside a standard "
                                         "capability (N6)"),
    ]
    for defect, step, description in defects:
        session, stub, files = _stub_session(mode="actions", timeout=timeout, defect=defect)
        try:
            broken = Report("selftest defect %s" % defect, quiet=True)
            run_steps(session, tempfile.gettempdir(), timeout, broken)
            session.shutdown()
            session.close()
            report.check("4.%s" % defect, "injected defect is caught — %s" % description,
                         step in broken.failed_steps,
                         "FAIL=%d failed steps=%s"
                         % (broken.failures, sorted(set(broken.failed_steps))))
        finally:
            _close_stub_session(session, files)

    return report.summary()


# --------------------------------------------------------------------------- main


def build_parser():
    parser = argparse.ArgumentParser(
        prog="lsp_client.py",
        description="Independent stdio LSP client and conformance harness for jev-lsp "
                    "(docs/VERIFICATION.md §1). Speaks Content-Length framed JSON-RPC over "
                    "the server's stdin/stdout, prints one ok/FAIL/skip/warn line per "
                    "assertion on stdout, all diagnostics on stderr, and exits 0 only when "
                    "every assertion passed.",
        epilog="Assertions are transcribed from PROTOCOL.md: §2 capabilities, §3.1 the "
               "edit-free fast path (N2), §3.5 progress tokens, §4 the action data shape, "
               "§8 the edit contract (N4), §9 pull diagnostics. Fixtures are written into "
               "a fresh temp dir under --workspace and removed unless --keep-fixtures.\n"
               "Exit codes: 0 all assertions passed; 1 at least one FAIL; 2 harness error.",
        formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--server", metavar="PATH",
                        help="path to the jev-lsp binary to drive over stdio "
                             "(required unless --selftest)")
    parser.add_argument("--workspace", metavar="DIR", default=os.getcwd(),
                        help="workspace root: initialize rootUri/workspaceFolders, the "
                             "server's cwd, and the parent of the fixture temp dir "
                             "(default: the current directory)")
    parser.add_argument("--timeout", metavar="SECONDS", type=float, default=DEFAULT_TIMEOUT,
                        help="per-request budget; a request that misses it fails its step "
                             "instead of hanging (default: %(default)s)")
    parser.add_argument("--stub-model-url", metavar="URL", default=None,
                        help="point every model tier at URL through the "
                             "workspace/configuration channel (PROTOCOL §10) and through "
                             "JEV_BASE_URL in the server's environment; without it the "
                             "client answers with `rules.enabled: false` (so the ambient pass "
                             "is the chat review this client is written against) and the "
                             "server keeps its own defaults for everything else")
    parser.add_argument("--keep-fixtures", action="store_true",
                        help="keep the fixture temp dir and log its path on stderr")
    parser.add_argument("--selftest", action="store_true",
                        help="prove this client without a server: run the framing, "
                             "correlation and server-request checks, all nine steps "
                             "against an in-process stub LSP server defined in this file, "
                             "and twelve injected contract defects that must each turn the "
                             "harness red; exit 0 when the client itself is correct")
    return parser


def main(argv=None):
    args = build_parser().parse_args(argv)

    if args.selftest:
        failures = run_selftest(max(args.timeout, 5.0))
        return 1 if failures else 0

    if not args.server:
        print("lsp_client.py: --server PATH is required (or use --selftest)",
              file=sys.stderr)
        return 2
    workspace = os.path.abspath(args.workspace)
    if not os.path.isdir(workspace):
        print("lsp_client.py: --workspace is not a directory: %s" % workspace,
              file=sys.stderr)
        return 2
    server = args.server if os.path.exists(args.server) else shutil.which(args.server)
    if not server:
        print("lsp_client.py: no such server: %s" % args.server, file=sys.stderr)
        return 2

    env = dict(os.environ)
    if args.stub_model_url:
        # Belt and braces: crates/jev-lsp documents JEV_BASE_URL for shells that already
        # know where the local model lives, while workspace/configuration (PROTOCOL §10) is
        # the normative channel this client answers on. The server re-applies the
        # environment after merging configuration, so the two cannot disagree.
        env["JEV_BASE_URL"] = args.stub_model_url
    root = pathlib.Path(workspace).resolve()
    session = None
    report = Report("lsp_client")
    try:
        process = subprocess.Popen([server], stdin=subprocess.PIPE,
                                   stdout=subprocess.PIPE, stderr=None,
                                   cwd=workspace, env=env)
    except OSError as exc:
        print("lsp_client.py: could not start %s: %s" % (server, exc), file=sys.stderr)
        return 2

    try:
        session = Session(process.stdout, process.stdin, timeout=args.timeout,
                          name=os.path.basename(server),
                          workspace_folders=[{"uri": root.as_uri(), "name": root.name}],
                          process=process)
        session.stub_model_url = args.stub_model_url
        run_steps(session, workspace, args.timeout, report,
                  keep_fixtures=args.keep_fixtures, server=server)
        session.shutdown()
    except (HarnessError, TransportError, FramingError, RequestTimeout) as exc:
        report.fail("harness", "the session ran to the end", str(exc))
        if exc.__cause__:
            log("cause:", exc.__cause__)
        traceback.print_exc(file=sys.stderr)
    finally:
        if session is not None:
            session.close()
        else:
            try:
                process.wait(timeout=2.0)
            except subprocess.TimeoutExpired:
                process.kill()
    failures = report.summary()
    return 1 if failures else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
