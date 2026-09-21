#!/usr/bin/env python3
"""The rules pass (`.jev/rules/*.json`), end to end, against the real server binary.

The ambient pass is a *rules* pass: an inspection finds candidate lines locally, one decision
call answers them all, and the confirmed ones go through the same `findings::build` the chat
review uses. This harness asserts each claim of that design, through the same `Lsp` client and
`Stub` that `smoke.py` provides — no second LSP client, no GPU, no network.

Each check writes its own revision of the rules, because the rules' hash is part of the cache
key: a revision is a different question, and that is what makes "exactly one decision call"
an assertion about this check rather than about whatever ran before it.

    python3 verify/rules_test.py [--bin target/release/jev-lsp]

Exits 0 only when every check passes. Every check runs, whatever happened before it.
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
sys.path.insert(0, HERE)

from smoke import Lsp, Stub, check, RESULTS  # noqa: E402

# One `.unwrap()` and two `TODO`s, each on a line whose text occurs exactly once, so an anchor
# can land on it without the needle growing.
FIXTURE = """fn a() {
    let x = p.unwrap();  // TODO: return the error
}
// TODO: document this
"""

PY_FIXTURE = """def load(path):
    return open(path)
"""

LICENCE = "// Copyright 2026\n"


def rule(rid, title, applies_to, inspection, **rest):
    body = {
        "id": rid,
        "title": title,
        "text": f"{title}.",
        "severity": "warning",
        "applies_to": [applies_to],
        "inspection": inspection,
        "judgement": {
            "question": f"Is {rid} a violation here?",
            "criteria": {"true": "it is reachable", "false": "it is not"},
            "reasons": {"reachable": "a request can reach it"},
            "min_probability": 0.75,
        },
        "verb_hint": "fix",
    }
    body.update(rest)
    return body


def rules_document(revision, *rules):
    return json.dumps(
        {
            "schema": "jev.rules/1",
            "rules": [dict(r, docs=f"revision {revision}") for r in rules],
        },
        indent=2,
    )


def todo_rule(max_matches):
    return rule(
        "todo-budget",
        "Too many TODOs",
        "**/*.rs",
        {"kind": "regex", "pattern": "TODO", "max_matches": max_matches},
    )


def unwrap_rule():
    return rule(
        "no-unwrap",
        "Unwrap in a handler",
        "**/*.rs",
        {"kind": "regex", "pattern": r"\.unwrap\(\)"},
    )


def header_rule():
    return rule(
        "has-copyright",
        "Missing copyright header",
        "**/*.py",
        {"kind": "absent", "pattern": "// Copyright"},
    )


def git(root, *args):
    return subprocess.run(
        ["git", "-C", root, *args],
        capture_output=True,
        text=True,
        env=dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null"),
    )


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default=os.path.join(REPO, "target", "release", "jev-lsp"))
    ap.add_argument("--keep", action="store_true", help="do not delete the fixture directory")
    args = ap.parse_args()
    RESULTS.clear()

    if not os.path.exists(args.bin):
        print(f"server binary not found: {args.bin}", file=sys.stderr)
        return 2

    stub = Stub()
    workdir = tempfile.mkdtemp(prefix="jev-rules-")
    rules_dir = os.path.join(workdir, ".jev", "rules")
    os.makedirs(rules_dir)
    fixture = os.path.join(workdir, "fixture.rs")
    headed_py = os.path.join(workdir, "headed.py")
    bare_py = os.path.join(workdir, "bare.py")
    for path, text in (
        (fixture, FIXTURE),
        (headed_py, LICENCE + PY_FIXTURE),
        (bare_py, PY_FIXTURE),
    ):
        with open(path, "w") as fh:
            fh.write(text)

    def script(value):
        """Set the stub's script. `{}` restores the defaults."""
        request = urllib.request.Request(
            stub.url("/__script"), data=json.dumps(value).encode(), method="POST"
        )
        request.add_header("content-type", "application/json")
        with urllib.request.urlopen(request, timeout=2) as r:
            r.read()

    def decisions():
        with urllib.request.urlopen(stub.url("/__decisions"), timeout=2) as r:
            return json.load(r)["decisions"]

    def write_rules(revision, *rules, broken=False):
        for name in os.listdir(rules_dir):
            os.remove(os.path.join(rules_dir, name))
        with open(os.path.join(rules_dir, "a.json"), "w") as fh:
            fh.write(rules_document(revision, *rules))
        if broken:
            with open(os.path.join(rules_dir, "broken.json"), "w") as fh:
                fh.write("{ this is not JSON")

    # Everything committed before the server starts, so the changed-set check has a repository
    # to answer from.
    git_ok = (
        git(workdir, "init", "-q").returncode == 0
        and git(workdir, "config", "user.email", "test@example.invalid").returncode == 0
        and git(workdir, "config", "user.name", "test").returncode == 0
    )

    env = dict(
        os.environ,
        JEV_BASE_URL=f"http://127.0.0.1:{stub.port}/v1",
        JEV_DECIDE_BASE_URL=f"http://127.0.0.1:{stub.port}/v1",
        JEV_MODEL="stub-model",
        JEV_REVIEW_MODEL="stub-model",
        JEV_DECIDE_MODEL="stub-model",
    )
    server = Lsp([args.bin, "--stdio"], env)
    # The harness makes many decision calls in a few seconds; the shipped per-minute ceiling is
    # sized for a person typing, and a refusal here would be the harness measuring itself.
    #
    # `rules.defaults` is off for these fixtures on purpose. Every fixture in this file means
    # **"only my rules ran"**: the counts below ("considered == 2", "loaded == 3") and the
    # "nothing to run" case (check 10) are about the rules *this* fixture wrote, and the shipped
    # set is a second source whose contents this file cannot know — a shipped rule claiming
    # `**/*.rs` or `**/*.md` makes every one of those wrong without anything being broken (that
    # is measured: three instances across this file and `cli_parity.py`, in the run that landed
    # the code group's `**/*.py` rules). Pinning is the durable form here because this harness is
    # LSP-only, so the setting can be sent to the one side there is.
    #
    # A fixture that means **"nothing applies to this file"** instead needs an extension nothing
    # ships a rule for — `cli_parity.py`'s `notes.zzz` and `nested/deep/mod.zzz` — because a
    # harness comparing the CLI against the LSP cannot pin one side and not the other: the CLI
    # reads no settings at all.
    #
    # What the shipped set does when a repository has written nothing is asserted in the crate
    # tests, over both a fixture set and the real embedded one
    # (`jev-lsp::engine::tests::a_repository_with_no_rules_of_its_own_is_inspected_by_the_shipped_set`,
    # `jev::run::tests::a_repository_with_no_rules_is_inspected_by_the_shipped_set_this_binary_carries`).
    server.settings = {
        "budget": {"max_calls_per_min": 500, "max_calls_per_hour": 2000},
        "rules": {"defaults": False},
    }
    uri = "file://" + fixture

    def inspect(path, force=True, timeout=30):
        result = server.request(
            "workspace/executeCommand",
            {"command": "jev.inspect", "arguments": [{"path": path, "force": force}]},
            timeout=timeout,
        ).get("result")
        if "considered" not in (result or {}):
            print(f"    (jev.inspect refused: {result})")
        return result or {}

    def open_doc(uri, language, text):
        server.notify(
            "textDocument/didOpen",
            {
                "textDocument": {
                    "uri": uri,
                    "languageId": language,
                    "version": 1,
                    "text": text,
                }
            },
        )

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
        open_doc(uri, "rust", FIXTURE)
        open_doc("file://" + headed_py, "python", LICENCE + PY_FIXTURE)
        open_doc("file://" + bare_py, "python", PY_FIXTURE)

        print("[rules] 1. a malformed rule file is skipped with a reason; the rest still load")
        write_rules(1, unwrap_rule(), todo_rule(2), header_rule(), broken=True)
        result = inspect(fixture)
        skipped = {s["code"]: s["detail"] for s in result.get("skipped", [])}
        check(
            any(p.endswith("broken.json") for p in skipped),
            f"the malformed file is named in skipped ({sorted(skipped)})",
        )
        check(
            any("not a rules document" in r for r in skipped.values()),
            f"and the reason says what was wrong ({list(skipped.values())})",
        )
        check(
            result.get("considered") == 2,
            f"the two rules that claim a .rs file still loaded "
            f"(considered={result.get('considered')})",
        )
        check(result.get("ok") is True, "and the pass answered rather than failing")

        print("[rules] 2. regex candidates over a fixture file")
        check(
            result.get("candidates") == 1,
            f"one `.unwrap()` line is a candidate (candidates={result.get('candidates')})",
        )
        findings = result.get("findings") or []
        check(len(findings) == 1, f"and one finding came back (got {len(findings)})")
        if findings:
            f = findings[0]
            check(
                f.get("line") == 1 and f.get("label") == "Unwrap in a handler",
                f"anchored on the matching line, labelled with the rule's title "
                f"({f.get('line')}, {f.get('label')!r})",
            )
            check(
                "p=0.90" in (f.get("detail") or ""),
                f"the decision's probability is in the detail ({f.get('detail')!r})",
            )
            check(
                "reachable" in (f.get("detail") or ""),
                "and the reason label the decision chose",
            )
            check(
                f.get("severity") == "warning" and f.get("verb") == "fix",
                f"severity and verb come from the rule ({f.get('severity')}, {f.get('verb')})",
            )

        print("[rules] 3. absent semantics, both ways")
        write_rules(2, unwrap_rule(), todo_rule(2), header_rule())
        present = inspect(headed_py)
        absent = inspect(bare_py)
        check(
            present.get("considered") == 1 and present.get("candidates") == 0,
            f"the pattern being present yields nothing "
            f"(considered={present.get('considered')}, candidates={present.get('candidates')})",
        )
        check(
            absent.get("considered") == 1 and absent.get("candidates") == 1,
            f"its absence yields exactly one candidate "
            f"(considered={absent.get('considered')}, candidates={absent.get('candidates')})",
        )
        absent_findings = absent.get("findings") or []
        check(
            len(absent_findings) == 1 and absent_findings[0].get("line") == 0,
            f"and the finding is at the head of the file, where the header belongs "
            f"({absent_findings})",
        )

        print("[rules] 4. max_matches, both ways")
        write_rules(3, unwrap_rule(), todo_rule(2), header_rule())
        at_allowance = inspect(fixture)
        write_rules(4, unwrap_rule(), todo_rule(1), header_rule())
        over = inspect(fixture)
        check(
            at_allowance.get("candidates") == 1,
            f"two TODOs against a threshold of two is not a repetition "
            f"(candidates={at_allowance.get('candidates')})",
        )
        check(
            over.get("candidates") == 3,
            f"one over the threshold reports both TODO lines and the unwrap "
            f"(candidates={over.get('candidates')})",
        )

        print("[rules] 5. an answer below min_probability publishes nothing")
        write_rules(5, unwrap_rule(), todo_rule(1), header_rule())
        script({"decision_probability": 0.4})
        before = len(decisions())
        low = inspect(fixture)
        check(low.get("candidates") == 3, f"the candidates were found ({low.get('candidates')})")
        check(
            not (low.get("findings") or []),
            f"and none of them became a finding ({low.get('findings')})",
        )
        check(
            len(decisions()) == before + 1,
            f"the question was still asked ({before} -> {len(decisions())})",
        )
        script({})

        print("[rules] 6. N candidates cause exactly one decision call")
        write_rules(6, unwrap_rule(), todo_rule(1), header_rule())
        before = len(decisions())
        confirmed = inspect(fixture)
        calls = decisions()
        check(
            len(calls) == before + 1,
            f"one call for the whole document ({before} -> {len(calls)})",
        )
        questions = calls[-1].get("questions") or {}
        check(
            len(questions) == 3,
            f"and it carried every candidate as a question ({len(questions)})",
        )
        check(
            set(questions) == {"no-unwrap#1", "todo-budget#1", "todo-budget#3"},
            f"named by rule and line ({sorted(questions)})",
        )
        check(
            "no-unwrap#1" in calls[-1].get("state", "")
            and calls[-1].get("state", "").count("CANDIDATES") == 1,
            "the state names the candidates the questions are about",
        )
        check(
            len(confirmed.get("findings") or []) == 3,
            f"and all three were confirmed (got {len(confirmed.get('findings') or [])})",
        )
        check(
            all(q.get("type") == "noul" for q in questions.values()),
            "every question is a noul question",
        )

        print("[rules] 7. an unchanged document is skipped without a call")
        if not git_ok:
            check(False, "check 7 needs git, and `git init` failed")
        else:
            git(workdir, "add", "-A")
            commit = git(workdir, "-c", "commit.gpgsign=false", "commit", "-q", "-m", "fixture")
            check(commit.returncode == 0, f"the fixture is committed ({commit.stderr.strip()})")
            # A fresh revision, so a cached conclusion cannot answer for the changed-set check.
            write_rules(7, unwrap_rule(), todo_rule(1), header_rule())
            before = len(decisions())
            unchanged = inspect(fixture, force=False)
            check(
                any(s.get("code") == "unchanged" for s in unchanged.get("skipped", [])),
                f"an untouched file is skipped as unchanged ({unchanged.get('skipped')})",
            )
            check(not (unchanged.get("findings") or []), "and nothing is reported for it")
            check(
                len(decisions()) == before,
                f"and no decision was made ({before} -> {len(decisions())})",
            )
            # The same document, insisted on, is inspected.
            forced = inspect(fixture, force=True)
            check(
                len(forced.get("findings") or []) == 3,
                f"and `--force` inspects it anyway ({len(forced.get('findings') or [])})",
            )

        print("[rules] 8. a second pass over identical content hits the cache")
        write_rules(8, unwrap_rule(), todo_rule(1), header_rule())
        before = len(decisions())
        first = inspect(fixture)
        after_first = len(decisions())
        second = inspect(fixture)
        check(after_first == before + 1, f"the first pass asks ({before} -> {after_first})")
        check(
            len(decisions()) == after_first,
            f"the second answers from the cache ({after_first} -> {len(decisions())})",
        )
        check(first.get("findings") == second.get("findings"), "and it answers the same thing")
        check(
            second.get("considered") == first.get("considered")
            and second.get("candidates") == first.get("candidates"),
            "considered and candidates survive a cache hit "
            f"({first.get('considered')}/{first.get('candidates')} -> "
            f"{second.get('considered')}/{second.get('candidates')})",
        )

        print("[rules] 9. a rule finding reaches the diagnostics pull, tagged with its source")
        # The ambient pass, on a save. The file has to be one git reports as changed, or the
        # pass is (correctly) skipped — so it is edited first, exactly as a user's save would.
        edited = FIXTURE + "\n"
        with open(fixture, "w") as fh:
            fh.write(edited)
        server.notify(
            "textDocument/didChange",
            {
                "textDocument": {"uri": uri, "version": 2},
                "contentChanges": [{"text": edited}],
            },
        )
        server.notify("textDocument/didSave", {"textDocument": {"uri": uri}})
        deadline = time.time() + 20
        items = []
        refreshes = 0
        while time.time() < deadline:
            seen = len(server.saw_request("workspace/diagnostic/refresh"))
            if seen > refreshes:
                refreshes = seen
                report = server.request(
                    "textDocument/diagnostic", {"textDocument": {"uri": uri}}
                ).get("result", {})
                items = report.get("items") or (
                    report.get("fullDocumentDiagnosticReport", {}) or {}
                ).get("items", [])
                if items:
                    break
            time.sleep(0.05)
        check(refreshes > 0, "the ambient pass asked the client to re-pull")
        check(len(items) == 3, f"the pull carries the rules findings (got {len(items)})")
        if items:
            check(
                all(i.get("source") == "jev" for i in items),
                "the diagnostic namespace stays jev (PROTOCOL §9)",
            )
            check(
                all((i.get("data") or {}).get("source") == "rules" for i in items),
                "and data.source names the pass that wrote them "
                f"({[(i.get('data') or {}).get('source') for i in items]})",
            )
            check(
                all((i.get("data") or {}).get("finding_id") for i in items),
                "every finding is still dismissible by a stable id",
            )

        # The push, not the pull: a client that one-shots `textDocument/diagnostic` before the
        # pass lands must still see the findings, so the completed pass publishes them itself
        # (PROTOCOL §9). The pull above proves the cache; this proves the notification.
        publish_diags = [
            (note.get("params") or {}).get("diagnostics") or []
            for note in server.saw_notification("textDocument/publishDiagnostics")
            if ((note.get("params") or {}).get("uri")) == uri
        ]
        pushed = next((d for d in publish_diags if len(d) == 3), [])
        check(
            bool(pushed),
            "the ambient pass pushes its findings with publishDiagnostics, no pull needed "
            f"(publish sizes: {[len(d) for d in publish_diags]})",
        )
        if pushed:
            check(
                all(i.get("source") == "jev"
                    and (i.get("data") or {}).get("source") == "rules"
                    for i in pushed),
                "the pushed findings carry the same shape as the pull",
            )

        status = server.request(
            "workspace/executeCommand", {"command": "jev.status", "arguments": []}
        ).get("result", {})
        rules = status.get("rules") or {}
        check(
            rules.get("loaded") == 3
            and rules.get("candidates", 0) > 0
            and rules.get("calls", 0) > 0,
            f"jev.status reports what the last pass did ({rules})",
        )
        check(
            isinstance(rules.get("hash"), str) and len(rules.get("hash")) == 64,
            f"including the rules' hash ({rules.get('hash')!r})",
        )

        print("[rules] 10. nothing to run is reported, never silent")
        # The demotion's edge, from the outside: no rule claims this file, so the pass has
        # nothing to run — and says so rather than reporting a clean document. The chat review
        # does not step in.
        notes = os.path.join(workdir, "notes.md")
        with open(notes, "w") as fh:
            fh.write("# notes\n")
        open_doc("file://" + notes, "markdown", "# notes\n")
        before = len(decisions())
        nothing = inspect(notes)
        check(
            nothing.get("considered") == 0 and not (nothing.get("findings") or []),
            f"no rule claims it, so nothing is reported "
            f"(considered={nothing.get('considered')}, findings={nothing.get('findings')})",
        )
        codes = [s.get("code") for s in nothing.get("skipped", [])]
        check("no_rules" in codes, f"and the pass says why ({nothing.get('skipped')})")
        check(
            any("nothing to run" in (s.get("detail") or "") for s in nothing.get("skipped", [])),
            f"in a sentence a user can act on ({nothing.get('skipped')})",
        )
        check(
            len(decisions()) == before,
            f"and no model call was made for it ({before} -> {len(decisions())})",
        )
        check(
            (server.request(
                "workspace/executeCommand", {"command": "jev.status", "arguments": []}
            ).get("result", {}).get("rules") or {}).get("enabled") is True,
            "the rules pass is on: this is the empty case, not a disabled one",
        )

        print("[rules] 11. a rules document that cannot work is reported, never inert")
        # The class the audit demonstrated on a real rule file: a pattern `regex` refuses finds no
        # candidates, so the rule is inert, and the failure is invisible from the outside — a
        # convention this repository keeps perfectly and a rule that never fires look the same.
        # `rules::lint` says both problems, and the pass carries them in `skipped` beside
        # `no_rules`; nothing about it is enforced (the rule still runs).
        broken_rule = {
            "id": "half-a-rule",
            "title": "A pattern that does not compile",
            "text": "The pattern is broken.",
            "severity": "warning",
            "applies_to": ["**/*.py"],
            # JSON-decodes to `\w*_key\s*\(`, which `regex` rejects: an unescaped `(` opens a
            # group. This is the shape a hand-written file actually arrives with.
            "inspection": {"kind": "regex", "pattern": r"\\w*_key\\s*\\("},
            "judgement": {"question": "Is it a violation?", "criteria": {"true": "yes", "false": "no"},
                          "min_probability": 0.75},
        }
        # The second rule shares the id and is otherwise sound, so the two problems are one
        # each: an uncompilable pattern, and an id two rules claim.
        write_rules(11, broken_rule, dict(broken_rule, title="The same id twice",
                                          inspection={"kind": "regex", "pattern": r"open\("}))
        before = len(decisions())
        broken = inspect(fixture, force=True)
        lint = [s for s in broken.get("skipped", []) if s.get("code") == "lint"]
        check(len(lint) == 2, f"both problems are reported ({broken.get('skipped')})")
        check(
            any("uncompilable regex" in (s.get("detail") or "") for s in lint),
            f"the one that makes the rule inert names itself ({lint})",
        )
        check(
            any("duplicate rule id" in (s.get("detail") or "") for s in lint),
            f"and so does the id two rules share ({lint})",
        )
        check(
            len(decisions()) == before,
            f"with no rule claiming this file, so no call ({before} -> {len(decisions())})",
        )
        check(
            ((server.request(
                "workspace/executeCommand", {"command": "jev.status", "arguments": []}
            ).get("result", {}).get("rules") or {}).get("lint")) == 2,
            "and jev.status counts them, so a repository that never reads `skipped` still sees "
            "a number where it had a silent rule",
        )

        print("[rules] 12. a rule's own problem survives the unchanged shortcut")
        # The sequence a user actually hits: fix the broken pattern, save, and watch an untouched
        # file change nothing. `lint` is a fact about the *rule set*, not about the document, so it
        # is reported on the shortcut too — where "unchanged" alone would be the last word.
        #
        # The file is one that did not exist when the changed set was last asked for: the server
        # reuses that answer for two seconds (`state::CHANGED_TTL`), and a file that was created
        # after it was taken cannot be in it. That makes the shortcut reachable here by
        # construction rather than by a sleep.
        if not git_ok:
            check(False, "check 12 needs git, and `git init` failed")
        else:
            # The uncompilable pattern fixed, the duplicate id left: exactly one problem, so a
            # count that agrees is a count of something.
            sound = {"kind": "regex", "pattern": r"open\("}
            write_rules(12, dict(broken_rule, inspection=sound),
                        dict(broken_rule, title="The same id twice", inspection=sound))
            untouched = os.path.join(workdir, "untouched.rs")
            with open(untouched, "w") as fh:
                fh.write("fn b() {}\n")
            open_doc("file://" + untouched, "rust", "fn b() {}\n")
            git(workdir, "add", "-A")
            commit = git(workdir, "-c", "commit.gpgsign=false", "commit", "-q", "-m", "rules")
            check(commit.returncode == 0,
                  f"the untouched fixture and the fixed rules are committed ({commit.stderr.strip()})")
            before = len(decisions())
            shortcut = inspect(untouched, force=False)
            skipped = shortcut.get("skipped", [])
            check(
                any(s.get("code") == "unchanged" for s in skipped),
                f"the untouched file is still reported unchanged ({skipped})",
            )
            check(
                any(s.get("code") == "lint" and "duplicate rule id" in (s.get("detail") or "")
                    for s in skipped),
                f"and the rule set's own problem is reported with it ({skipped})",
            )
            check(
                len(decisions()) == before,
                f"with no model call for it ({before} -> {len(decisions())})",
            )
            # `jev.status` reports what the last *pass* did, and a shortcut is not a pass — so the
            # count is read after a real one over the same rule set, and must agree with the
            # message the shortcut just sent.
            inspect(untouched, force=True)
            status = server.request(
                "workspace/executeCommand", {"command": "jev.status", "arguments": []}
            ).get("result", {})
            check(
                ((status.get("rules") or {}).get("lint")) == 1,
                f"and jev.status counts exactly that one problem ({status.get('rules')})",
            )

        print("[rules] 13. a pull with no pass behind it runs one, instead of answering clean")
        # A client that never saves has no pass behind it: an editor whose edits arrive as
        # `didChange`, a harness that writes the file itself. An empty answer to that pull is
        # indistinguishable from a clean file — the one thing a checker must not be — so the pull
        # starts the rules pass. No `didSave` is sent here, which is the whole point.
        script({})  # the stub's defaults: every question answered `true`, at 0.9
        write_rules(2, unwrap_rule())
        later = FIXTURE + "\n// one more line, so the content hash moves\n"
        with open(fixture, "w") as fh:
            fh.write(later)
        server.notify(
            "textDocument/didChange",
            {
                "textDocument": {"uri": uri, "version": 3},
                "contentChanges": [{"text": later}],
            },
        )
        before = len(decisions())
        # The pull starts the pass and answers from what is cached. The findings arrive with the
        # refresh that pass sends, the same way they do after a save — so the second pull below is
        # the one that carries them.
        server.request("textDocument/diagnostic", {"textDocument": {"uri": uri}}, timeout=30)
        deadline = time.time() + 20
        items = []
        refreshes = 0
        while time.time() < deadline:
            seen = len(server.saw_request("workspace/diagnostic/refresh"))
            if seen > refreshes:
                refreshes = seen
                report = server.request(
                    "textDocument/diagnostic", {"textDocument": {"uri": uri}}, timeout=30
                ).get("result", {})
                items = report.get("items") or (
                    report.get("fullDocumentDiagnosticReport", {}) or {}
                ).get("items", [])
                if items:
                    break
            time.sleep(0.05)
        asked = len(decisions())
        check(asked == before + 1, f"the pull ran exactly one pass ({before} -> {asked})")
        check(len(items) == 1, f"and its finding reaches the pull (got {len(items)})")
        if items:
            check(
                all(i.get("source") == "jev" for i in items),
                "the diagnostic namespace stays jev (PROTOCOL §9)",
            )
            check(
                all((i.get("data") or {}).get("source") == "rules" for i in items),
                f"and data.source names the pass that wrote them "
                f"({[(i.get('data') or {}).get('source') for i in items]})",
            )
        server.request("textDocument/diagnostic", {"textDocument": {"uri": uri}}, timeout=30)
        check(
            len(decisions()) == asked,
            f"a second pull of the same content costs nothing ({asked} -> {len(decisions())})",
        )

        server.request("shutdown", None)
        server.notify("exit", None)
        return 0 if all(ok for ok, _ in RESULTS) else 1
    finally:
        server.stop()
        stub.stop()
        if not args.keep:
            shutil.rmtree(workdir, ignore_errors=True)
        passed = sum(1 for ok, _ in RESULTS if ok)
        print(f"\n[rules] {passed}/{len(RESULTS)} checks passed")


if __name__ == "__main__":
    sys.exit(main())
