#!/usr/bin/env python3
"""A scripted OpenAI-compatible endpoint for tests.

It answers `POST /v1/chat/completions` with deterministic JSON derived from the request,
so the whole pipeline (HTTP, contract parsing, anchor resolution, LSP edit shape) can be
exercised without a GPU and without network access.

Rules it applies, in order:
  * the system prompt asks for findings  -> a review response anchored on STUB_ANCHOR
  * the system prompt asks for markdown  -> an artifact
  * otherwise                            -> an edit response replacing the anchor's scope

Control plane, for assertions:
  * `GET  /__requests` -> every request body seen, in order
  * `GET  /__script`   -> the current script
  * `POST /__script`   -> replace the script with a JSON object; keys are
                          `review`, `edit`, `artifact`, or `raw` (a verbatim string),
                          optionally with `times` to serve it a limited number of times
  * `POST /__reset`    -> forget everything

Environment:
  STUB_PORT    port to bind (default 8099)
  STUB_ANCHOR  text the generated responses anchor on (default "File::open")
  STUB_RAW     if set, every response body is this string verbatim

Standard library only.
"""
import json
import os
import re
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse

ANCHOR = os.environ.get("STUB_ANCHOR")
RAW = os.environ.get("STUB_RAW")
# Milliseconds to stall before answering, so a test can land an edit while a model call
# is in flight and exercise the analysis queue rather than the cache.
DELAY_MS = int(os.environ.get("STUB_DELAY_MS", "0"))

_ANCHOR_RE = re.compile(r"\nCODE:\n(.*?)(?:\nCONTEXT \(do not modify\):|\Z)", re.S)


# How long to wait between streamed pieces. Long enough that a server coalescing its output
# still emits several partials, so a test can tell streaming from buffering.
STREAM_DELAY_MS = int(os.environ.get("STUB_STREAM_DELAY_MS", "150"))

# Answer a `stream: true` request with one ordinary JSON object, the way a server that does not
# implement streaming does. The client must fall back rather than report an empty answer.
IGNORE_STREAM = os.environ.get("STUB_IGNORE_STREAM") == "1"


def anchor_from_request(body):
    """Pick an anchor that actually exists in the document under discussion.

    A fixed anchor only works for one fixture in one language, which makes the stub lie
    about what a real model does. Deriving it from the CODE block keeps the stub honest:
    the anchor is always present, always unique enough, and the replacement is visible.
    """
    if ANCHOR:
        return ANCHOR
    text = "\n".join(
        m.get("content", "") for m in body.get("messages", []) if isinstance(m, dict)
    )
    m = _ANCHOR_RE.search(text)
    code = m.group(1) if m else text
    candidates = [
        line for line in code.split("\n")
        if line.strip() and not line.strip().startswith(("//", "#", "*", "/*"))
    ]
    if not candidates:
        return ""
    # The longest line is the most likely to occur exactly once.
    return max(candidates, key=len)


def _edit_body(anchor):
    # Language-agnostic on purpose: the stub must not inject Rust into a Python fixture, or
    # a test asserting on the result would be asserting the stub's shape, not the server's.
    return {
        "summary": "mark the line the model reasoned about",
        "rationale": "Scripted response: the anchor is the longest line of the scope.",
        "replacements": [
            {
                "anchor": {"kind": "statement", "match": anchor.strip()},
                "replacement": anchor.rstrip() + "  # meta",
            }
        ],
        "new_files": [],
    }


def _review_body(anchor):
    return {
        "findings": [
            {
                "anchor": {"kind": "statement", "match": anchor.strip()},
                "severity": "warning",
                "label": "scripted finding",
                "detail": "The scripted model always reports the anchor line.",
                "verb_hint": "fix",
            }
        ]
    }

LOCK = threading.Lock()
STATE = {"requests": [], "script": {}, "served": []}



def _artifact_body():
    return {
        "summary": "what this does",
        "markdown": "# Summary\n\nReads a file and discards the error.",
    }


def _completion_body(anchor):
    """Plain text, not JSON: a completion is inserted verbatim."""
    stripped = anchor.strip()
    return stripped + "_completed"


def _plan_body(anchor):
    """A one-step plan aimed at whatever the request's scope is."""
    return {
        "goal": "scripted goal",
        "steps": [
            {
                "title": "mark the anchor line",
                "rationale": "Scripted: the plan points at the longest line of the scope.",
                "verb": "harden",
                "anchors": [{"kind": "statement", "match": anchor.strip()}],
            }
        ],
    }


def choose(wants_findings, wants_markdown, anchor, wants_plan=False):
    """Return (content, source) where source names the rule that fired."""
    with LOCK:
        script = dict(STATE["script"])
    for key in ("raw", "review", "edit", "artifact", "plan"):
        entry = script.get(key)
        if not entry:
            continue
        entry, remaining = _entry(entry)
        if remaining is not None and remaining <= 0:
            continue
        with LOCK:
            STATE["served"].append(key)
        return entry, key

    if RAW is not None:
        return RAW, "raw-env"
    if wants_findings:
        return json.dumps(_review_body(anchor)), "rule:review"
    if wants_plan:
        return json.dumps(_plan_body(anchor)), "rule:plan"
    if wants_markdown:
        return json.dumps(_artifact_body()), "rule:artifact"
    return json.dumps(_edit_body(anchor)), "rule:edit"


def _entry(entry):
    """Normalise a script entry to (json_text, remaining_times_or_None)."""
    if isinstance(entry, str):
        return entry, None
    if isinstance(entry, dict):
        times = entry.get("times")
        payload = entry.get("body", entry)
        if times is not None:
            with LOCK:
                used = STATE.setdefault("times_used", {})
                key = json.dumps(payload, sort_keys=True)
                used[key] = used.get(key, 0) + 1
                return json.dumps(payload), max(0, times - used[key] + 1)
        return json.dumps(payload), None
    return json.dumps(entry), None


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):  # keep the test output quiet
        pass

    def _send(self, code, payload, ctype="application/json"):
        body = payload.encode("utf-8") if isinstance(payload, str) else json.dumps(payload).encode()
        self.send_response(code)
        self.send_header("content-type", ctype)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _send_stream(self, content, source):
        """Answer with SSE, the way llama.cpp does: `data:` events, a keep-alive comment, a
        final usage chunk with no choices, then `[DONE]`.

        Delimited by connection close (HTTP/1.1 allows it when `Connection: close` is sent), so
        no chunked framing to get wrong here. The pieces are paced so a server coalescing its
        output still produces more than one partial: with everything sent at once there would
        be exactly one flush and the test could not tell streaming from buffering.
        """
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("connection", "close")
        self.close_connection = True
        self.end_headers()

        def event(payload):
            self.wfile.write(b"data: " + json.dumps(payload).encode() + b"\n\n")
            self.wfile.flush()

        self.wfile.write(b": keep-alive\n\n")  # a comment line is not content
        self.wfile.flush()

        # Split on whitespace boundaries so each piece is a plausible token group.
        words = content.split(" ")
        pieces = [w + (" " if i < len(words) - 1 else "") for i, w in enumerate(words)]
        step = max(1, len(pieces) // 4 or 1)
        for i in range(0, len(pieces), step):
            event(
                {
                    "choices": [
                        {
                            "index": 0,
                            "delta": {"content": "".join(pieces[i : i + step])},
                            "finish_reason": None,
                        }
                    ]
                }
            )
            time.sleep(STREAM_DELAY_MS / 1000.0)
        event({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})
        event(
            {
                "choices": [],
                "usage": {"prompt_tokens": 11, "completion_tokens": 7},
                "_stub_source": source,
            }
        )
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()

    def do_GET(self):
        path = urlparse(self.path).path
        if path == "/__requests":
            with LOCK:
                self._send(200, {"requests": STATE["requests"], "served": STATE["served"]})
        elif path == "/__script":
            with LOCK:
                self._send(200, STATE["script"])
        elif path in ("/health", "/__health"):
            self._send(200, {"ok": True})
        else:
            self._send(404, {"error": "not found"})

    def do_POST(self):
        length = int(self.headers.get("content-length") or 0)
        raw = self.rfile.read(length) if length else b""
        path = urlparse(self.path).path

        if path == "/__reset":
            with LOCK:
                STATE["requests"].clear()
                STATE["script"].clear()
                STATE["served"].clear()
                STATE.pop("times_used", None)
            self._send(200, {"reset": True})
            return

        if path == "/__script":
            try:
                script = json.loads(raw or b"{}")
            except json.JSONDecodeError as e:
                self._send(400, {"error": str(e)})
                return
            with LOCK:
                STATE["script"] = script
                STATE.pop("times_used", None)
            self._send(200, {"script": script})
            return

        if path.endswith("/chat/completions"):
            if DELAY_MS:
                time.sleep(DELAY_MS / 1000.0)
            try:
                body = json.loads(raw or b"{}")
            except json.JSONDecodeError as e:
                self._send(400, {"error": str(e)})
                return
            with LOCK:
                STATE["requests"].append(body)

            text = " ".join(
                m.get("content", "") for m in body.get("messages", []) if isinstance(m, dict)
            )
            wants_findings = '"findings"' in text
            wants_plan = '"steps"' in text and not wants_findings
            wants_markdown = '"markdown"' in text and not wants_findings and not wants_plan
            wants_completion = "completion engine" in text
            if wants_completion:
                # A completion needs no anchor: it is answered from the cursor position.
                # (The request was already recorded above; do not count it twice.)
                self._send(200, {
                    "id": "stub",
                    "object": "chat.completion",
                    "model": body.get("model", "stub"),
                    "choices": [{"index": 0, "finish_reason": "stop",
                                 "message": {"role": "assistant",
                                             "content": "return_value"}}],
                    "usage": {"prompt_tokens": 5, "completion_tokens": 3},
                    "_stub_source": "rule:completion",
                })
                return
            anchor = anchor_from_request(body)
            if not anchor:
                self._send(200, {"error": "no anchor available"})
                return
            content, source = choose(wants_findings, wants_markdown, anchor, wants_plan)
            if body.get("stream") and not IGNORE_STREAM:
                self._send_stream(content, source)
                return
            self._send(
                200,
                {
                    "id": "stub",
                    "object": "chat.completion",
                    "model": body.get("model", "stub"),
                    "choices": [
                        {
                            "index": 0,
                            "message": {"role": "assistant", "content": content},
                            "finish_reason": "stop",
                        }
                    ],
                    "usage": {"prompt_tokens": 11, "completion_tokens": 7},
                    "_stub_source": source,
                },
            )
            return

        self._send(404, {"error": "not found"})


def main():
    port = int(os.environ.get("STUB_PORT", "8099"))
    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    print(f"stub model listening on http://127.0.0.1:{port}/v1", file=sys.stderr, flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
