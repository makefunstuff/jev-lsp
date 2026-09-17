#!/usr/bin/env python3
"""Independent probe: an edit that lands during an analysis is never lost.

Found by `verify/lsp_client.py` (step 8) while the shared stub model was saturated: after
`textDocument/codeAction` opened the ambient pass, an edit plus a save arrived while that
model call was still running. The save's pass was dropped silently — no log, no retry — so
the content the client held never got a conclusion and the server never sent
`workspace/diagnostic/refresh` for it. The client's pull was then correctly empty, which is
indistinguishable from a clean document: a user-visible hole, not a slow path.

`verify/queue_test.py` asserts this from the server's side with an injected defect.
This probe asserts the same contract from the *other* direction, through the independent
client's own primitives, and states it as the two things a client can observe:

  * a refresh arrives after the save, even though the run it superseded was interrupted;
  * the conclusion the client pulls afterwards describes the *current* content — proven by
    content identity (the pull's `resultId` moved with the edit) and by the sweep of items,
    not by timing.

A superseded pass refreshes too, and that request can arrive before the pass for the
content the client now holds; so the probe re-pulls on every refresh, bounded, exactly as a
client must. One pull after one refresh is what loses findings.

The stub is stalled (`--stall-ms`, default 1500) so the race is deterministic rather than
load-dependent, and the probe asserts the race was actually set up (`analysis_in_flight`)
so a green run cannot mean "nothing was in flight".

    python3 verify/supersede_probe.py [--server target/release/meta-lsp] [--stall-ms 1500]

Standard library only. Exits 0 when every assertion holds.
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
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
sys.path.insert(0, HERE)

import lsp_client  # noqa: E402 — the independent client, reused rather than reimplemented

FIXTURE = lsp_client.FIXTURE_PY
# A real edit that keeps the file parseable and keeps every anchor line present.
EDITED = FIXTURE.replace("for attempt in range(attempts):",
                         "for attempt in range(1, attempts + 1):")
assert EDITED != FIXTURE, "the probe fixture did not change; the edit is not exercising it"


def log(*parts):
    print("[supersede]", *parts, file=sys.stderr, flush=True)


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def start_stub(port, stall_ms):
    """Start verify/stub_model.py, stalled by `stall_ms` on every completion, and wait for
    the port to accept. Its stderr is inherited so a stub failure is visible."""
    env = dict(os.environ, STUB_PORT=str(port), STUB_DELAY_MS=str(stall_ms))
    proc = subprocess.Popen([sys.executable, os.path.join(HERE, "stub_model.py")],
                            cwd=REPO, env=env)
    deadline = time.monotonic() + 20.0
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            raise RuntimeError("stub_model.py exited with %s" % proc.returncode)
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                return proc
        except OSError:
            time.sleep(0.1)
    proc.terminate()
    raise RuntimeError("stub_model.py never accepted on port %d" % port)


def code_action_params(uri):
    line = next(index for index, text in enumerate(FIXTURE.split("\n"))
                if "for attempt in range" in text)
    return {"textDocument": {"uri": uri},
            "range": {"start": {"line": line, "character": 4},
                      "end": {"line": line, "character": 4}},
            "context": {"diagnostics": [], "triggerKind": 1}}


def diagnostic_params(uri):
    return {"textDocument": {"uri": uri}, "identifier": "meta"}


def run_case(server, stub_url, timeout, racy):
    """Open, optionally open the action menu, edit, save — then watch what the client sees."""
    workspace = tempfile.mkdtemp(prefix="meta-supersede-")
    fixture = os.path.join(workspace, "attention.py")
    pathlib.Path(fixture).write_text(FIXTURE, encoding="utf-8", newline="\n")
    proc = subprocess.Popen([server], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            cwd=REPO, env=dict(os.environ, META_BASE_URL=stub_url))
    session = lsp_client.Session(proc.stdout, proc.stdin, timeout=timeout, process=proc)
    result = {"racy": racy}
    try:
        session.request("initialize", lsp_client.initialize_params(REPO))
        session.notify("initialized", {})
        uri = session.did_open(fixture, "python")
        result["before_id"] = (session.request("textDocument/diagnostic",
                                               diagnostic_params(uri)) or {}).get("resultId")
        if racy:
            session.request("textDocument/codeAction", code_action_params(uri))
            status = session.request("workspace/executeCommand",
                                     {"command": "meta.status", "arguments": [{}]}) or {}
            result["in_flight"] = status.get("analysis_in_flight")
        baseline = session.server_request_count("workspace/diagnostic/refresh")
        session.did_change(uri, EDITED)
        session.did_save(uri)
        # A superseded pass refreshes too, and that request can arrive before the pass for
        # the content we now hold. So: re-pull on every new request, until the report
        # describes the current content or the budget runs out.
        deadline = time.monotonic() + timeout
        refreshes, items, after_id = 0, 0, None
        while time.monotonic() < deadline:
            remaining = deadline - time.monotonic()
            if not session.wait_for(
                    lambda: session.server_request_count("workspace/diagnostic/refresh") > baseline,
                    remaining, "workspace/diagnostic/refresh after the save"):
                break
            refreshes += session.server_request_count("workspace/diagnostic/refresh") - baseline
            baseline = session.server_request_count("workspace/diagnostic/refresh")
            report = session.request("textDocument/diagnostic", diagnostic_params(uri)) or {}
            items = len(report.get("items") or [])
            after_id = report.get("resultId")
            if items:
                break
        result["refreshed"] = refreshes > 0
        result["refreshes"] = refreshes
        result["after_id"] = after_id
        result["items"] = items
        result["logs"] = [m["params"].get("message")
                          for m in session.notifications("window/logMessage")]
    finally:
        session.shutdown()
        session.close()
        shutil.rmtree(workspace, ignore_errors=True)
    return result


def main():
    parser = argparse.ArgumentParser(
        prog="supersede_probe.py",
        description="Probe that an edit landing during an in-flight analysis is not lost "
                    "(the hole verify/lsp_client.py step 8 caught). Asserted from the "
                    "client's side: a refresh arrives after the save and the pull that "
                    "follows describes the current content.")
    parser.add_argument("--server", metavar="PATH", default=os.path.join(
        REPO, "target", "release", "meta-lsp"), help="meta-lsp binary (default: "
        "target/release/meta-lsp)")
    parser.add_argument("--stall-ms", metavar="MS", type=int, default=1500,
                        help="delay the stub model applies to every completion, so the "
                             "race is deterministic (default: %(default)s)")
    parser.add_argument("--timeout", metavar="SECONDS", type=float, default=20.0,
                        help="per-request and per-wait budget (default: %(default)s)")
    args = parser.parse_args()

    if not os.path.exists(args.server):
        print("supersede_probe.py: no such server: %s" % args.server, file=sys.stderr)
        return 2

    report = lsp_client.Report("supersede_probe")
    port = free_port()
    stub_url = "http://127.0.0.1:%d/v1" % port
    stub = None
    try:
        stub = start_stub(port, args.stall_ms)
        print("== probe: edit during an in-flight analysis, stub stalled by %d ms"
              % args.stall_ms)

        print("\n-- control: open -> edit -> save (no analysis in flight)")
        control = run_case(args.server, stub_url, args.timeout, racy=False)
        report.info("control", "refresh=%s items=%d resultId %s -> %s"
                    % (control["refreshed"], control["items"],
                       str(control["before_id"])[:14], str(control["after_id"])[:14]))
        report.check("control", "a refresh arrives after the save (§3.4/§9)",
                     control["refreshed"], _detail(control))
        report.check("control", "the pull carries findings for the edited content (§9)",
                     control["items"] >= 1, _detail(control))
        report.check("control", "the pull's resultId moved with the edit",
                     control["after_id"] is not None
                     and control["after_id"] != control["before_id"], _detail(control))

        print("\n-- race: open -> codeAction (pass starts) -> edit -> save")
        race = run_case(args.server, stub_url, args.timeout, racy=True)
        report.info("race", "in_flight=%s refresh=%s items=%d resultId %s -> %s"
                    % (race["in_flight"], race["refreshed"], race["items"],
                       str(race["before_id"])[:14], str(race["after_id"])[:14]))
        report.check("race", "the race was actually set up: a pass was still in flight "
                             "when the edit landed", race["in_flight"] is True,
                     "analysis_in_flight=%s" % race["in_flight"])
        report.check("race", "a refresh arrives after the save, though the interrupted run "
                             "was superseded (§3.4/§9)", race["refreshed"], _detail(race))
        report.check("race", "the pull carries findings for the edited content (§9)",
                     race["items"] >= 1, _detail(race))
        report.check("race", "the pull's resultId moved with the edit — the conclusion "
                             "describes the current content",
                     race["after_id"] is not None
                     and race["after_id"] != race["before_id"], _detail(race))
        report.info("race", "same content hash as the control run: %s"
                    % (race["after_id"] == control["after_id"]))
        return 1 if report.summary() else 0
    except (lsp_client.HarnessError, lsp_client.TransportError,
            lsp_client.FramingError, lsp_client.RequestTimeout, RuntimeError) as exc:
        report.fail("harness", "the probe ran to the end", str(exc))
        report.summary()
        return 2
    finally:
        if stub is not None:
            stub.terminate()
            try:
                stub.wait(timeout=5)
            except subprocess.TimeoutExpired:
                stub.kill()


def _detail(case):
    def show(value):
        try:
            return json.dumps(value)[:110]
        except (TypeError, ValueError):
            return repr(value)[:110]

    return ("refreshes=%d items=%d before=%s after=%s log=%s"
            % (case.get("refreshes", 0), case["items"], str(case["before_id"])[:14],
               str(case["after_id"])[:14], show(case["logs"])))


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
