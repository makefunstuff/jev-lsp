#!/usr/bin/env python3
"""Minimal LSP server used by verify/probes/streaming.lua.

Purpose: prove the client-initiated progress path end to end — that a client-supplied
`workDoneToken` reaches the server in the request params, and that `$/progress`
notifications under that token reach the client's `LspProgress` autocmd.

It deliberately implements nothing else. Standard library only.
"""
import json
import sys

TOKEN_SEEN = {}
SHUTDOWN = False


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


def notify(method, params):
    write({"jsonrpc": "2.0", "method": method, "params": params})


def progress(token, kind, **extra):
    value = {"kind": kind}
    value.update(extra)
    notify("$/progress", {"token": token, "value": value})


def main():
    while True:
        msg = read_message()
        if msg is None:
            return
        method = msg.get("method")
        msg_id = msg.get("id")

        if method == "initialize":
            write({"jsonrpc": "2.0", "id": msg_id, "result": {
                "capabilities": {
                    "positionEncoding": "utf-8",
                    "textDocumentSync": {"openClose": True, "change": 2},
                    "executeCommandProvider": {
                        "commands": ["probe.emit"],
                        "workDoneProgress": True,
                    },
                },
                "serverInfo": {"name": "meta-streaming-probe", "version": "0"},
            }})
        elif method == "initialized":
            pass
        elif method == "workspace/executeCommand":
            params = msg.get("params") or {}
            token = params.get("workDoneToken")
            # The whole point: is the token visible in the request params?
            if token is not None:
                progress(token, "begin", title="probe", percentage=0)
                progress(token, "report", message="halfway", percentage=50)
                progress(token, "end", message="done")
            write({"jsonrpc": "2.0", "id": msg_id, "result": {
                "sawWorkDoneToken": token is not None,
                "token": token,
                "command": params.get("command"),
            }})
        elif method == "shutdown":
            write({"jsonrpc": "2.0", "id": msg_id, "result": None})
        elif method == "exit":
            return
        elif msg_id is not None:
            write({"jsonrpc": "2.0", "id": msg_id,
                   "error": {"code": -32601, "message": "not implemented: %s" % method}})


if __name__ == "__main__":
    main()
