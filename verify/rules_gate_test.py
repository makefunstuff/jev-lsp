#!/usr/bin/env python3
"""verify/rules_gate_test.py — the gate's exits, proved rather than assumed.

`verify/rules-gate.sh` is what puts `.jev/rules/` in the loop for a CLI agent: it runs the
shipped `jev inspect --force` over a scope and exits non-zero when a rule fires. Its most
important exit is **2** — a gate that cannot do its job must never look like a pass — and the
subtle half of that is the scope: a run that scanned nothing has checked nothing, and "nothing
was scanned" must not read as "the rules ran and were quiet". This harness checks every exit
against a fixture repository, so a later edit cannot quietly turn any of them green.

Asserts, in order:

  1. a fixture file with a seeded violation exits **1**, and the finding is printed as
     `path:line  label  (p=…)`;
  2. a fixture file no rule claims exits **0** with no finding;
  3. no key at all exits **2** with the reason, and prints no finding;
  4. an endpoint nothing answers on exits **2** with the transport reason, when a rule has
     something to ask that endpoint — the case that must never read as "clean";
  5. a path with no candidate asks the endpoint nothing and exits **0** (nothing needed a model);
  6. an empty scope — a clean tree and no `--all` — exits **2**, with a reason that says which
     scope came up empty, and that reason on stderr as well as stdout;
  7. `--all` scans the tree for real: a real file count, and the seeded violation found;
  8. a path that matches nothing exits **2** for the same reason as an empty scope.

The decide tier comes from the environment (`JEV_DECIDE_*`, as `verify/run-suite.sh` exports
them, pointed at the stub). The key is only needed for the gate's own check: the stub ignores
auth, so any value works, and no value is printed.

The scope checks (6, 7, 8) run a byte-identical copy of the gate inside the fixture repository:
the gate deliberately scopes itself to *its own* repository (`REPO` comes from the script's
location), so a fixture scope can only be exercised by a copy that lives there.

Prints ok/FAIL per check. Exit is nonzero only on FAIL.
"""
import os
import shutil
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.realpath(__file__)))
GATE = os.path.join(REPO, "verify", "rules-gate.sh")
BIN = os.path.join(REPO, "target", "release", "jev")

FAILS = []
SKIPS = []


def say(line):
    print(line, flush=True)


def check(cond, label, detail=None):
    if cond:
        say("ok    " + label)
    else:
        FAILS.append(label)
        say("FAIL  " + label + (("  — " + str(detail)) if detail else ""))
    return cond


def gate(gate_path, args, env_extra, cwd):
    env = dict(os.environ)
    env.update({"JEV_GATE_BIN": BIN})
    env.update(env_extra)
    proc = subprocess.run(["bash", gate_path] + args, capture_output=True, text=True, env=env, cwd=cwd)
    return proc.returncode, proc.stdout.strip(), proc.stderr.strip()


def main():
    workdir = tempfile.mkdtemp(prefix="rules-gate-")
    try:
        # A fixture that is a real repository: the CLI resolves the rules from the file's git root,
        # and the gate's default scope is a git query against its own repository.
        os.makedirs(os.path.join(workdir, ".git"))
        shutil.copytree(os.path.join(REPO, ".jev", "rules"), os.path.join(workdir, ".jev", "rules"))
        seeded = os.path.join(workdir, "crates", "demo", "src", "handler.rs")
        # The negative control. It means **"nothing fires on this file"**, not "nothing applies
        # to it": a `.rs` file *is* claimed by this repository's own rules, and by any shipped
        # rule that claims `**/*.rs`. It is clean because no rule's pattern matches its text.
        # The gate has no settings channel — it runs the CLI, and the CLI reads no settings — so
        # a shipped rule that ever matches this fixture breaks the control rather than the
        # mechanism: this file is then what needs changing, to text no shipped pattern matches.
        clean = os.path.join(workdir, "crates", "demo", "src", "clean.rs")
        os.makedirs(os.path.dirname(seeded))
        with open(seeded, "w") as fh:
            fh.write("use std::fs;\n\npub fn handle(path: &str) -> String {\n"
                     "    let body = fs::read_to_string(path).unwrap();\n    body\n}\n")
        with open(clean, "w") as fh:
            fh.write("//! A fixture every rule lets through.\n\npub fn body(t: &str) -> &str {\n    t\n}\n")

        # The copy the scope checks need (same bytes; it just lives in the fixture repository).
        # It is created before the commit so that a "clean tree" really is one.
        os.makedirs(os.path.join(workdir, "verify"))
        gate_copy = os.path.join(workdir, "verify", "rules-gate.sh")
        shutil.copyfile(GATE, gate_copy)

        git = shutil.which("git")
        committed = False
        if git:
            for step in (["init", "-q"], ["add", "-A"],
                         ["-c", "user.email=test@example.invalid", "-c", "user.name=test",
                          "commit", "-q", "-m", "fixture"]):
                subprocess.run([git, "-C", workdir] + step, capture_output=True, text=True)
            committed = subprocess.run([git, "-C", workdir, "rev-parse", "--verify", "HEAD"],
                                       capture_output=True, text=True).returncode == 0

        stub = os.environ.get("JEV_DECIDE_BASE_URL", "")
        if not stub:
            say("rules_gate: JEV_DECIDE_BASE_URL is unset, so the gate cannot be run")
            return 2

        base_env = {"JEV_DECIDE_BASE_URL": stub,
                    "JEV_DECIDE_WIRE": os.environ.get("JEV_DECIDE_WIRE", "system_one"),
                    "JEV_DECIDE_MODEL": os.environ.get("JEV_DECIDE_MODEL", "stub-model"),
                    "TYPESAFE_API_KEY": "the-stub-does-not-check-this"}

        rc, out, err = gate(GATE, [seeded], base_env, workdir)
        check(rc == 1, "a seeded violation exits 1", "exit %s: %s" % (rc, err or out))
        check("Unwrap outside tests" in out and "(p=" in out,
              "the finding is printed as path:line  label  (p=…)", out or err)
        check("rules-gate:" in out, "the counts are printed beside it", out)
        for line in out.splitlines():
            if line.startswith("rules-gate:"):
                say("      " + line)

        rc, out, err = gate(GATE, [clean], base_env, workdir)
        check(rc == 0, "a file no rule claims exits 0", "exit %s: %s" % (rc, out or err))
        check("(p=" not in out, "and prints no finding", out)

        no_key = dict(base_env)
        no_key.pop("TYPESAFE_API_KEY")
        no_key["JEV_GATE_KEY_FILE"] = os.path.join(workdir, "absent.key")
        rc, out, err = gate(GATE, [clean], no_key, workdir)
        check(rc == 2, "no key exits 2", "exit %s" % rc)
        check("no decide key" in err, "and says which key it wanted (stderr)", err)

        rc, out, err = gate(GATE, [clean], base_env, workdir)
        check(rc == 0, "a path with no candidate asks the endpoint nothing and exits 0",
              "exit %s: %s" % (rc, err or out))

        dead = dict(base_env, JEV_DECIDE_BASE_URL="http://127.0.0.1:9/v1")
        rc, out, err = gate(GATE, [seeded], dead, workdir)
        check(rc == 2, "an endpoint nothing answers exits 2 when a rule has a question", "exit %s" % rc)
        check("did not answer" in err or "transport" in err, "and says the transport failed", err)

        # --- the scope: nothing scanned is a failure, not a pass -------------------------------
        if not committed:
            SKIPS.append("the scope checks need a committed fixture repository and git")
            say("SKIP  the scope checks need a committed fixture repository and git")
        else:
            rc, out, err = gate(gate_copy, [], base_env, workdir)
            check(rc == 2, "an empty scope exits 2, not 0", "exit %s: %s" % (rc, out or err))
            check("nothing was scanned" in out, "and says so on stdout", out)
            check("nothing was scanned" in err, "and on stderr", err)
            check("no file differs" in err, "and names which scope was empty", err)
            if "nothing was scanned" in out:
                say("      " + out.splitlines()[0])

            rc, out, err = gate(gate_copy, ["--all"], base_env, workdir)
            counts = [l for l in out.splitlines() if l.startswith("rules-gate:") and "file(s)" in l]
            check(rc == 1, "--all scans the tree and finds the seeded violation", "exit %s: %s" % (rc, out or err))
            scanned = int(counts[0].split()[1]) if counts else 0
            check(bool(counts) and scanned >= 3 and seeded in out,
                  "--all reports a real file count and still finds the seeded file",
                  counts or out)
            if counts:
                say("      " + counts[0])

            rc, out, err = gate(gate_copy, ["crates/demo/src/not-a-file.rs"], base_env, workdir)
            check(rc == 2, "a path that matches nothing exits 2", "exit %s: %s" % (rc, out or err))
            check("nothing was scanned" in err and "missing" in err,
                  "and the reason says the path was missing", err)

        say("[rules_gate] %d failure(s), %d skip(s)" % (len(FAILS), len(SKIPS)))
        return 1 if FAILS else 0
    finally:
        shutil.rmtree(workdir, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
