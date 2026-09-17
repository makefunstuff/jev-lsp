#!/usr/bin/env python3
"""Reference implementation of the frozen contract's U1 loop, used as evidence by
verify/probes/trace.lua.

It implements, against the real client and over real stdio framing:
  initialize            -> the exact capabilities from PROTOCOL.md §2
  didOpen / didChange   -> document version tracking
  textDocument/codeAction -> actions with NO edit, carrying `data` (§4, non-negotiable N2)
  codeAction/resolve      -> a WorkspaceEdit with documentChanges + explicit version (§8, N4)
  textDocument/diagnostic -> findings carrying data.finding_id (§9)
  workspace/diagnostic/refresh -> sent as a server->client request after didOpen

It is deliberately small: this is the message-shape proof, not the product. If a payload
here is rejected or ignored by the client, the frozen contract is wrong and this file
fails loudly rather than the Rust server failing quietly later.

Standard library only.
"""
import json
import sys

DOCS = {}          # uri -> {"version": int, "text": str}
LAST_ACTION = {}   # uri -> version at the time the action was produced
REFRESH_SENT = 0
REFRESH_ACKED = 0


def log(*parts):
    print("[stub]", *parts, file=sys.stderr, flush=True)


def read_message():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":")[1])
    if length is None:
        return None
    return json.loads(sys.stdin.buffer.read(length))


def write(payload):
    body = json.dumps(payload).encode("utf-8")
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()


def respond(msg_id, result):
    write({"jsonrpc": "2.0", "id": msg_id, "result": result})


def request(msg_id_from_us, method, params):
    write({"jsonrpc": "2.0", "id": msg_id_from_us, "method": method, "params": params})


def make_action(uri):
    """An action with no edit: the fast path (N2). Stable title: deterministic (§4)."""
    version = DOCS.get(uri, {}).get("version", 0)
    LAST_ACTION[uri] = version
    return {
        "title": "meta: harden parse",           # f(verb, scope) — not model output
        "kind": "refactor.rewrite.meta",
        "isPreferred": True,
        "data": {
            "v": 1,
            "id": "action-1",
            "verb": "harden",
            "state": "ready",
            "doc": {"uri": uri, "version": version, "content_hash": "sha256:stub"},
            "scope": {"kind": "function", "name": "parse"},
        },
    }


def resolve(action):
    """Fill the edit. Stamps the version the action was created against (§8 rule 3)."""
    uri = action.get("data", {}).get("doc", {}).get("uri")
    stamped = LAST_ACTION.get(uri)
    action["edit"] = {
        "documentChanges": [{
            "textDocument": {"uri": uri, "version": stamped},   # explicit int, never absent
            "edits": [{
                "range": {"start": {"line": 0, "character": 0},
                          "end": {"line": 0, "character": 0}},
                "newText": "-- meta: applied\n",
            }],
        }],
    }
    return action


def main():
    global REFRESH_SENT, REFRESH_ACKED
    while True:
        msg = read_message()
        if msg is None:
            return
        method = msg.get("method")
        msg_id = msg.get("id")

        # Responses to requests we sent (the refresh ack).
        if method is None and msg_id is not None and "result" in msg:
            REFRESH_ACKED += 1
            continue

        if method == "initialize":
            respond(msg_id, {
                "capabilities": {
                    "positionEncoding": "utf-8",
                    "textDocumentSync": {"openClose": True, "change": 2},
                    "codeActionProvider": {
                        "resolveProvider": True,
                        "codeActionKinds": ["quickfix", "quickfix.meta",
                                            "refactor.rewrite", "refactor.rewrite.meta",
                                            "source", "source.meta", "source.fixAll"],
                    },
                    "diagnosticProvider": {"identifier": "meta",
                                           "interFileDependencies": False,
                                           "workspaceDiagnostics": True},
                    "executeCommandProvider": {"commands": ["meta.status"],
                                               "workDoneProgress": True},
                },
                "serverInfo": {"name": "meta-trace-stub", "version": "0"},
            })

        elif method == "initialized":
            pass

        elif method == "textDocument/didOpen":
            doc = msg["params"]["textDocument"]
            DOCS[doc["uri"]] = {"version": doc.get("version", 1), "text": doc.get("text", "")}
            # Server-initiated refresh: proves the push->pull loop (PROTOCOL §3.4).
            REFRESH_SENT += 1
            request(1000 + REFRESH_SENT, "workspace/diagnostic/refresh", None)

        elif method == "textDocument/didChange":
            doc = msg["params"]["textDocument"]
            DOCS.setdefault(doc["uri"], {})["version"] = doc.get("version", 0)

        elif method == "textDocument/codeAction":
            uri = msg["params"]["textDocument"]["uri"]
            respond(msg_id, [make_action(uri)])

        elif method == "codeAction/resolve":
            respond(msg_id, resolve(msg["params"]))

        elif method == "textDocument/diagnostic":
            uri = msg["params"]["textDocument"]["uri"]
            version = DOCS.get(uri, {}).get("version", 0)
            respond(msg_id, {
                "kind": "full",
                "resultId": "diag-%d" % version,
                "items": [{
                    "range": {"start": {"line": 0, "character": 0},
                              "end": {"line": 0, "character": 5}},
                    "severity": 2,
                    "source": "meta",
                    "message": "unchecked error path",
                    "data": {"finding_id": "finding-1", "verb": "fix",
                             "content_hash": "sha256:stub"},
                }],
                "refreshSent": REFRESH_SENT,
                "refreshAcked": REFRESH_ACKED,
            })

        elif method == "shutdown":
            respond(msg_id, None)
        elif method == "exit":
            return
        elif msg_id is not None:
            respond(msg_id, None)


if __name__ == "__main__":
    main()
