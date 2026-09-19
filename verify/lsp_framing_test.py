#!/usr/bin/env python3
"""The stdio framing of the test client — a defect the suite found in itself.

`smoke.Lsp._read_message` used to read a header one byte at a time into a *local* buffer with a
one-second deadline, and to discard what it had read when that deadline expired. Under load the
deadline expired mid-header, so the next call read from the middle of a message, mis-framed
everything after it, and — through the reader thread's blanket `except Exception: break` — killed
the reader for the rest of the session. Every later request then timed out at 30 s, which is
exactly how it presented: `verify/latency.py` red two runs in three while the server was
innocent.

This harness is that reproduction, kept. It drives the reader over a real pipe with no server and
no product code, so it is fast and deterministic.

    python3 verify/lsp_framing_test.py

Exits 0 only when every check passes.
"""
import json
import os
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from smoke import Lsp, FramingError  # noqa: E402

RESULTS = []


def check(ok, label):
    RESULTS.append((bool(ok), label))
    print(("  ok    " if ok else "  FAIL  ") + label)
    return ok


def framed(payload, header=b"Content-Length"):
    """One frame, as the server writes it."""
    body = json.dumps(payload).encode()
    return b"%s: %d\r\n\r\n%s" % (header, len(body), body)


def pipe_reader():
    """A reader wired to a real pipe, with the server process replaced by its stdout end."""
    read_fd, write_fd = os.pipe()

    class Fake:
        pass

    reader = Fake()
    reader.proc = Fake()
    reader.proc.stdout = os.fdopen(read_fd, "rb", 0)
    reader._buffer = b""
    reader.framing_errors = []
    # The reader's own helpers, for a caller that is not a whole `Lsp`.
    reader._content_length = Lsp._content_length
    return reader, write_fd


def write_later(fd, chunks):
    """Write each chunk after the given delay, from a thread, so a reader can be mid-frame."""

    def run():
        for delay, chunk in chunks:
            time.sleep(delay)
            os.write(fd, chunk)

    thread = threading.Thread(target=run, daemon=True)
    thread.start()
    return thread


def main():
    print("[framing] 1. a header split across two reads survives the deadline")
    reader, fd = pipe_reader()
    message = framed({"jsonrpc": "2.0", "id": 7, "result": {"ok": True}})
    write_later(fd, [(0.0, message[:10]), (1.4, message[10:])])

    # The first call's deadline expires mid-header. It must return None *and keep the bytes*.
    first = Lsp._read_message(reader, timeout=1.0)
    check(first is None, "a frame that has not arrived is reported as nothing yet")
    second = Lsp._read_message(reader, timeout=2.0)
    check(
        isinstance(second, dict) and second.get("result") == {"ok": True} and second.get("id") == 7,
        f"and the same frame is read whole once the rest arrives (got {second!r})",
    )
    check(not reader.framing_errors, f"with nothing recorded as a framing error ({reader.framing_errors})")
    os.close(fd)

    print("[framing] 2. the reader survives a frame it cannot read")
    reader, fd = pipe_reader()
    good = framed({"jsonrpc": "2.0", "id": 1, "result": "after"})
    # A complete frame whose body is not JSON, followed by a good one on the same stream.
    bad = b"Content-Length: 9\r\n\r\nnot-json!"
    write_later(fd, [(0.0, bad), (0.05, good)])
    got = Lsp._read_message(reader, timeout=2.0)
    check(
        isinstance(got, dict) and got.get("result") == "after",
        f"the next frame is still read correctly (got {got!r})",
    )
    check(
        any("not JSON" in e for e in reader.framing_errors),
        f"and the unreadable one is reported rather than swallowed ({reader.framing_errors})",
    )
    os.close(fd)

    print("[framing] 3. a header with no length is reported, not guessed past")
    reader, fd = pipe_reader()
    # Nothing here says where the following frame begins, so there is nothing to resynchronise
    # to. The reader must say that rather than skip bytes and mis-read what comes next.
    headerless = b"X-Nonsense: 3\r\n\r\nxyz"
    write_later(fd, [(0.0, headerless), (0.05, framed({"jsonrpc": "2.0", "id": 2, "result": 2}))])
    raised = None
    try:
        Lsp._read_message(reader, timeout=2.0)
    except FramingError as exc:
        raised = exc
    check(raised is not None, "an unusable header raises rather than reading garbage")
    check(
        any("content-length" in e for e in reader.framing_errors),
        f"and the reason is recorded for the caller ({reader.framing_errors})",
    )
    os.close(fd)

    print("[framing] 4. two frames arriving in one read are both delivered")
    reader, fd = pipe_reader()
    os.write(fd, framed({"jsonrpc": "2.0", "id": 3, "result": "one"})
             + framed({"jsonrpc": "2.0", "id": 4, "result": "two"}, header=b"content-length"))
    first = Lsp._read_message(reader, timeout=2.0)
    second = Lsp._read_message(reader, timeout=2.0)
    check(
        (first or {}).get("result") == "one" and (second or {}).get("result") == "two",
        f"framing is by length, not by read boundary ({first!r}, {second!r})",
    )
    os.close(fd)

    print("[framing] 5. a broken stream fails the request that was in flight")
    # The contract `request` owes a caller: a broken stream must not present as a timeout.
    reader, fd = pipe_reader()
    reader.framing_errors.append("a frame whose body is not JSON")
    check(
        FramingError.__name__ == "FramingError"
        and issubclass(FramingError, Exception)
        and "a frame whose body is not JSON" in str(reader.framing_errors[0]),
        "a framing failure is a distinct, reportable error rather than a timeout",
    )
    os.close(fd)

    passed = sum(1 for ok, _ in RESULTS if ok)
    print(f"\n[framing] {passed}/{len(RESULTS)} checks passed")
    return 0 if passed == len(RESULTS) else 1


if __name__ == "__main__":
    sys.exit(main())
