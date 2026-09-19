#!/usr/bin/env python3
"""Records the `languageId` of every document the client opens.

Used by verify/probes/language.lua. Answers one command, `probe.languageIds`, with the
map it recorded, so the probe can compare what the client *said* against what Neovim's own
detector *would* say. Standard library only.
"""
import json
import sys

LANGIDS = {}
OPENED = []


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
                    "executeCommandProvider": {"commands": ["probe.languageIds"]},
                },
                "serverInfo": {"name": "jev-language-probe", "version": "0"},
            }})
        elif method == "initialized":
            pass
        elif method == "textDocument/didOpen":
            doc = msg["params"]["textDocument"]
            LANGIDS[doc["uri"]] = doc.get("languageId")
            OPENED.append(doc["uri"])
        elif method == "workspace/executeCommand":
            write({"jsonrpc": "2.0", "id": msg_id,
                   "result": {"languageIds": LANGIDS, "opened": OPENED}})
        elif method == "shutdown":
            write({"jsonrpc": "2.0", "id": msg_id, "result": None})
        elif method == "exit":
            return
        elif msg_id is not None:
            write({"jsonrpc": "2.0", "id": msg_id, "result": None})


if __name__ == "__main__":
    main()
