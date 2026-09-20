#!/usr/bin/env python3
"""verify/rules_gate_test.py — the gate's four exits, proved rather than assumed.

`verify/rules-gate.sh` is what puts `.jev/rules/` in the loop for a CLI agent: it runs the
shipped `jev inspect --force` over a path list and exits non-zero when a rule fires. Its most
important exit is the third one — a gate that cannot run must never look like a pass — and this
harness checks all four against a fixture, so a later edit to the gate cannot quietly turn a
missing endpoint into a green row.

Asserts, in order:

  1. a fixture file with a seeded violation exits **1**, and the finding is printed as
     `path:line  label  (p=…)`;
  2. a fixture file no rule claims exits **0** with no finding;
  3. no key at all exits **2** with the reason, and prints no finding;
  4. an endpoint nothing answers on exits **2** with the transport reason, when a rule has
     something to ask that endpoint — the case that must never read as "clean". (A path with no
     candidate asks nothing, so it cannot make a dead endpoint visible; that is asserted
     separately, as exit 0 with zero candidates.)

The decide tier comes from the environment (`JEV_DECIDE_*`, as `verify/run-suite.sh` exports
them, pointed at the stub). The key is only needed for the gate's own check: the stub ignores
auth, so any value works, and the value never leaves this process.

Prints ok/FAIL per check. Exit is nonzero only on FAIL.
"""
import os
import shutil
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.realpath(__file__)))
GATE = os.path.join(REPO, "verify", "rules-gate.sh")

FAILS = []


def say(line):
    print(line, flush=True)


def check(cond, label, detail=None):
    if cond:
        say("ok    " + label)
    else:
        FAILS.append(label)
        say("FAIL  " + label + (("  — " + str(detail)) if detail else ""))
    return cond


def gate(args, env_extra, cwd):
    env = dict(os.environ)
    env.update(env_extra)
    proc = subprocess.run(["bash", GATE] + args, capture_output=True, text=True, env=env, cwd=cwd)
    return proc.returncode, proc.stdout.strip(), proc.stderr.strip()


def main():
    workdir = tempfile.mkdtemp(prefix="rules-gate-")
    try:
        # A fixture that is a repository: the CLI resolves the rules from the file's git root,
        # which is the nearest `.git`, so these are the rules the gate would use in the tree.
        os.makedirs(os.path.join(workdir, ".git"))
        shutil.copytree(os.path.join(REPO, ".jev", "rules"), os.path.join(workdir, ".jev", "rules"))
        seeded = os.path.join(workdir, "crates", "demo", "src", "handler.rs")
        clean = os.path.join(workdir, "crates", "demo", "src", "clean.rs")
        os.makedirs(os.path.dirname(seeded))
        with open(seeded, "w") as fh:
            fh.write("use std::fs;\n\npub fn handle(path: &str) -> String {\n"
                     "    let body = fs::read_to_string(path).unwrap();\n    body\n}\n")
        with open(clean, "w") as fh:
            fh.write("//! A fixture every rule lets through.\n\npub fn body(t: &str) -> &str {\n    t\n}\n")

        stub = os.environ.get("JEV_DECIDE_BASE_URL", "")
        if not stub:
            # Not a pass and not a skip: without an endpoint this harness measures nothing, and a
            # run that measured nothing must not report success. The gate's own contract is the
            # same exit for the same reason, and the suite's row supplies the stub, so this is
            # reached only when someone runs the harness by hand.
            say("rules_gate: JEV_DECIDE_BASE_URL is unset, so the gate cannot be run")
            return 2

        base_env = {"JEV_DECIDE_BASE_URL": stub,
                    "JEV_DECIDE_WIRE": os.environ.get("JEV_DECIDE_WIRE", "system_one"),
                    "JEV_DECIDE_MODEL": os.environ.get("JEV_DECIDE_MODEL", "stub-model"),
                    "TYPESAFE_API_KEY": "the-stub-does-not-check-this"}

        rc, out, err = gate([seeded], base_env, workdir)
        check(rc == 1, "a seeded violation exits 1", "exit %s: %s" % (rc, err or out))
        check("Unwrap outside tests" in out and "(p=" in out,
              "the finding is printed as path:line  label  (p=…)", out or err)
        check("rules-gate:" in out, "the counts are printed beside it", out)
        for line in out.splitlines():
            if line.startswith("rules-gate:"):
                say("      " + line)

        rc, out, err = gate([clean], base_env, workdir)
        check(rc == 0, "a file no rule claims exits 0", "exit %s: %s" % (rc, out or err))
        check("(p=" not in out, "and prints no finding", out)

        no_key = dict(base_env)
        no_key.pop("TYPESAFE_API_KEY")
        no_key["JEV_GATE_KEY_FILE"] = os.path.join(workdir, "absent.key")
        rc, out, err = gate([clean], no_key, workdir)
        check(rc == 2, "no key exits 2", "exit %s" % rc)
        check("no decide key" in err, "and says which key it wanted (stderr)", err)

        rc, out, err = gate([clean], base_env, workdir)
        check(rc == 0, "a path with no candidate asks the endpoint nothing and exits 0",
              "exit %s: %s" % (rc, err or out))

        dead = dict(base_env, JEV_DECIDE_BASE_URL="http://127.0.0.1:9/v1")
        rc, out, err = gate([seeded], dead, workdir)
        check(rc == 2, "an endpoint nothing answers exits 2 when a rule has a question", "exit %s" % rc)
        check("did not answer" in err or "transport" in err, "and says the transport failed", err)

        say("[rules_gate] %d failure(s), 0 skip(s)" % len(FAILS))
        return 1 if FAILS else 0
    finally:
        shutil.rmtree(workdir, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
