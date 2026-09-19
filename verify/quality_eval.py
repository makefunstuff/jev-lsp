#!/usr/bin/env python3
"""Does the review actually find the thing that is wrong?

`real_model.py` and `soak.py` answer "is the output usable" — does it parse, does the edit
apply, does the file still compile. Neither answers "is it right", which is the question every
context change in this project has been justified by reasoning rather than measurement. This is
the smallest thing that answers it: a handful of files with a defect planted in each, a couple
with nothing wrong, and a count of how often the review finds the first and stays quiet about
the second.

    python3 verify/quality_eval.py --base-url http://127.0.0.1:37313/v1 --model qwen3.6-35b-a3b-iq3xxs

What it measures:

  * **recall** — a defective file where a finding landed within two lines of the planted defect
    and said something about it. The keyword lists are deliberately generous: the metric is
    about whether the review *notices*, not whether it phrases things the way I would.
  * **precision** — of every finding produced, how many were at a planted defect. A review that
    reports something on every line would score perfect recall and be useless.
  * **quiet on clean files** — how many findings a file with nothing wrong attracts. This is the
    number that hurts in practice: a false positive costs a dismissal and some trust.

The fixtures are small and the defects are unambiguous on purpose. A metric that needs a rubric
to interpret is not a metric.
"""
import argparse
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
sys.path.insert(0, HERE)

from smoke import Lsp  # noqa: E402

# Each entry: the file, whether it is defective, the line the defect is on (1-based), and words
# a finding about it would plausibly use.
#
# The labels are hand-written and therefore the first thing to suspect when a verdict looks
# wrong: two of these lines were wrong for three runs, and the metric reported 50% recall for a
# review that had found all four defects. Nothing here can check them — the keywords describe
# the consequence (`leak`), not the line (`f = open(path)`) — so the fixture's line numbers are
# read by hand when a number changes, and the run prints every finding it kept.
FIXTURES = [
    {
        "name": "leaked_handle.py",
        "defect": True,
        "line": 5,
        "keywords": ["close", "context manager", "leak", "with"],
        "text": """import json


def load(path):
    f = open(path)
    return json.load(f)["port"]
""",
    },
    {
        "name": "swallowed_error.py",
        "defect": True,
        "line": 8,
        "keywords": ["except", "swallow", "silent", "ignore", "catch"],
        "text": """import json


def load(path):
    try:
        with open(path) as f:
            return json.load(f)
    except Exception:
        pass
""",
    },
    {
        "name": "mutable_default.py",
        "defect": True,
        # `def record(name, seen=[])` — line 1, and the label said 4 for three runs.
        "line": 1,
        "keywords": ["mutable", "default", "shared", "list"],
        "text": """def record(name, seen=[]):
    seen.append(name)
    return seen
""",
    },
    {
        "name": "unchecked_index.py",
        "defect": True,
        # `return ports[0]` — line 3.
        "line": 3,
        "keywords": ["index", "range", "empty", "check", "bound"],
        "text": """def first_port(services):
    ports = [s["port"] for s in services]
    return ports[0]
""",
    },
    {
        "name": "clean_reader.py",
        "defect": False,
        "line": 0,
        "keywords": [],
        "text": """import json


def load(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)["port"]
""",
    },
    {
        "name": "clean_writer.py",
        "defect": False,
        "line": 0,
        "keywords": [],
        "text": """import json


def save(path, payload):
    with open(path, "w", encoding="utf-8") as f:
        json.dump(payload, f)
""",
    },
]


def analysed(trace_path, uri):
    """Has the server finished an analysis of this document?

    Read from the server's own record rather than inferred from a notification: the entry
    names the uri and is written when the analysis completes, which is exactly the signal a
    measurement needs and exactly what an accumulating `saw_request` cannot provide.
    """
    if not os.path.isfile(trace_path):
        return False
    try:
        with open(trace_path) as fh:
            for line in fh:
                try:
                    entry = json.loads(line)
                except Exception:
                    continue
                if entry.get("kind") == "analysis" and entry.get("uri") == uri:
                    return True
    except OSError:
        return False
    return False


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default=os.path.join(REPO, "target", "release", "jev-lsp"))
    ap.add_argument("--base-url", required=True, help="an OpenAI-compatible endpoint")
    ap.add_argument("--model", required=True)
    ap.add_argument("--timeout", type=float, default=180)
    ap.add_argument("--think", default=None, choices=["off", "low", "medium", "high"],
                    help="override models.<tier>.think for both tiers. The metric is the "
                         "review tier's, but a level is worth setting on both: an action "
                         "resolved with reasoning and a review without is not a fair "
                         "comparison of anything")
    args = ap.parse_args()

    workdir = tempfile.mkdtemp(prefix="jev-quality-")
    trace_path = os.path.join(workdir, ".git", "jev", "session.jsonl")
    env = {
        k: v
        for k, v in os.environ.items()
        if not k.startswith("JEV_")
    }
    env["JEV_BASE_URL"] = args.base_url
    env["JEV_MODEL"] = args.model
    env["JEV_REVIEW_MODEL"] = args.model

    server = Lsp([args.bin, "--stdio"], env)
    # One analysis per file and this run is not about the limiter, so the ceiling is raised the
    # way a client raises it rather than through an environment variable that does not exist.
    tiers = {
        "reason": {"base_url": args.base_url, "model": args.model},
        "review": {"base_url": args.base_url, "model": args.model},
    }
    if args.think:
        for tier in tiers.values():
            tier["think"] = args.think
    server.settings = {
        "budget": {"max_calls_per_min": 120, "max_calls_per_hour": 600},
        "models": tiers,
    }
    findings_total = 0
    matched_total = 0
    defective = [f for f in FIXTURES if f["defect"]]
    clean = [f for f in FIXTURES if not f["defect"]]
    caught = 0
    noise = 0

    try:
        server.request(
            "initialize",
            {
                "processId": os.getpid(),
                "rootUri": "file://" + workdir,
                "capabilities": {"workspace": {"configuration": True}},
            },
        )
        server.notify("initialized", {})

        print(f"model: {args.model} at {args.base_url}")
        print(f"{'file':<24} {'findings':>8}  verdict")
        for fixture in FIXTURES:
            path = os.path.join(workdir, fixture["name"])
            with open(path, "w") as fh:
                fh.write(fixture["text"])
            uri = "file://" + path
            server.notify(
                "textDocument/didOpen",
                {
                    "textDocument": {
                        "uri": uri,
                        "languageId": "python",
                        "version": 1,
                        "text": fixture["text"],
                    }
                },
            )
            server.notify("textDocument/didSave", {"textDocument": {"uri": uri}})

            # Wait for *this* file's analysis, keyed on the server's own record rather than on
            # an LSP notification. `saw_request` accumulates, so "has a refresh ever arrived"
            # is true forever after the first file — that bug reported five files as misses
            # before the seventh analysis had started, and produced a headline in this
            # repository's log that had to be retracted. The record names the document, so the
            # wait is attributable and the number is about the file in front of it.
            deadline = time.time() + args.timeout
            items = []
            while time.time() < deadline:
                if analysed(trace_path, uri):
                    report = server.request(
                        "textDocument/diagnostic", {"textDocument": {"uri": uri}}
                    ).get("result", {})
                    items = report.get("items") or (
                        report.get("fullDocumentDiagnosticReport", {}) or {}
                    ).get("items", [])
                    break
                time.sleep(0.2)
            if not items and time.time() >= deadline:
                print(f"{fixture['name']:<24} {'-':>8}  no analysis within {args.timeout:.0f}s")

            findings_total += len(items)
            hit = False
            for item in items:
                line = item["range"]["start"]["line"] + 1
                message = (item.get("message") or "").lower()
                if fixture["defect"]:
                    near = abs(line - fixture["line"]) <= 2
                    says_so = any(word in message for word in fixture["keywords"])
                    if near and says_so:
                        hit = True
                        matched_total += 1
                else:
                    if message.strip():
                        pass  # counted below as noise
            if fixture["defect"] and hit:
                caught += 1
            if not fixture["defect"]:
                noise += len(items)

            # A miss is not one thing: the model can fail to notice, or it can notice and have
            # its answer discarded because the anchor could not be located in the file. The
            # second is a defect in this system, the first is a defect in the model, and a
            # metric that cannot tell them apart cannot tell you what to fix.
            discarded = 0
            for message in server.saw_notification("window/logMessage"):
                text = message["params"].get("message", "")
                if "discarded because their anchors" in text:
                    discarded += int(text.split()[1] or 0)
            if fixture["defect"]:
                if hit:
                    verdict = "caught"
                elif discarded:
                    verdict = f"NOT LOCATED ({discarded} finding(s) discarded)"
                else:
                    verdict = "MISSED"
            else:
                verdict = "quiet" if not items else f"{len(items)} finding(s) on a clean file"
            print(f"{fixture['name']:<24} {len(items):>8}  {verdict}")
            # What it actually said, because "MISSED" and "a finding about something else" are
            # different results and only one of them is about the defect that was planted.
            for item in items[:2]:
                line = item["range"]["start"]["line"] + 1
                message = (item.get("message") or "").replace("\n", " ")[:88]
                print(f"{'':<24} {'':>8}  line {line}: {message}")
            server.notify("textDocument/didClose", {"textDocument": {"uri": uri}})

        # What the server itself recorded, which is where a finding goes missing: the analysis
        # can produce findings and still leave none in the diagnostics — discarded on an anchor
        # that cannot be located, or refused before the call.
        trace = trace_path
        if os.path.isfile(trace):
            print()
            print("server-side record:")
            with open(trace) as fh:
                for line in fh:
                    try:
                        entry = json.loads(line)
                    except Exception:
                        continue
                    if entry.get("kind") == "analysis":
                        # `findings` is the list the server kept (line, severity, label); the
                        # count is its length, or the older scalar field on entries written
                        # before that list existed.
                        listed = entry.get("findings")
                        kept = len(listed) if isinstance(listed, list) else entry.get("count")
                        print(
                            "  {uri:<32} findings={findings} discarded={discarded}".format(
                                uri=entry.get("uri", "").rsplit("/", 1)[-1],
                                findings=kept,
                                discarded=entry.get("discarded"),
                            )
                        )

        print()
        recall = caught / len(defective) if defective else 0.0
        precision = matched_total / findings_total if findings_total else 0.0
        print(f"recall    {caught}/{len(defective)} defective files caught  ({recall:.0%})")
        print(
            f"precision {matched_total}/{findings_total} findings at a planted defect "
            f"({precision:.0%})"
        )
        print(f"clean     {noise} finding(s) across {len(clean)} files with nothing wrong")
        print()
        print(
            "This is a small metric on unambiguous defects. It is here to make a change to "
            "context or prompts measurable before it is believed, not to be a benchmark."
        )
        return 0
    finally:
        server.stop()
        shutil.rmtree(workdir, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
