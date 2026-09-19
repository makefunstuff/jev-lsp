#!/usr/bin/env python3
"""How many findings does this thing produce on a real repository?

    python3 verify/repo_bench.py --repo /data/jpl/Work/jev-lsp \
        --bin target/release/jev-lsp \
        --model-url http://127.0.0.1:4000/v1 --model deepseek/deepseek-v4-flash --limit 40

The quality metric (verify/quality_eval.py) answers "is the review *right*" on six small files.
This answers the other half — *how much comes out* — on files nobody wrote for a test: the
number a user actually feels, because a finding they did not want costs a dismissal and some
trust. It is a measurement, not a threshold: it prints and exits 0.

It shares no server code. The model is whatever `--model-url` points at, and a stub would
measure the stub, so a real endpoint is required; the extension table below is the one piece of
the server's vocabulary copied here on purpose, because a bench that asked the server which
extensions it supports would be measuring the server's opinion of itself.

Per file: `didOpen`, `didSave`, then wait for that exact document's `analysis` entry in the
record (`<root>/.git/jev/session.jsonl`), bounded at 60 s, and read the findings it lists.
Files whose analyses never land are reported as such — a timeout is data, not a silent zero.
"""

import argparse
import json
import os
import sys
import time
import urllib.request
from collections import Counter

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
sys.path.insert(0, HERE)

from smoke import Lsp  # noqa: E402

# Mirrors `jev_core::lang::from_extension`. Data, not code: the bench decides what to open by
# extension the way a user decides what to edit, and the language id it sends is the same word
# Neovim would send.
EXT_LANG = {
    "rs": "rust",
    "py": "python",
    "pyi": "python",
    "go": "go",
    "js": "javascript",
    "mjs": "javascript",
    "cjs": "javascript",
    "jsx": "javascript",
    "ts": "typescript",
    "tsx": "typescript",
    "mts": "typescript",
    "c": "c",
    "h": "c",
    "cc": "cpp",
    "cpp": "cpp",
    "cxx": "cpp",
    "hpp": "cpp",
    "hh": "cpp",
    "java": "java",
    "cs": "csharp",
    "rb": "ruby",
    "php": "php",
    "lua": "lua",
    "sh": "shell",
    "bash": "shell",
    "zsh": "shell",
    "fish": "shell",
    "sql": "sql",
    "md": "markdown",
    "markdown": "markdown",
    "mdx": "markdown",
    "json": "json",
    "jsonc": "json",
    "yaml": "yaml",
    "yml": "yaml",
    "toml": "toml",
    "xml": "xml",
    "svg": "xml",
    "html": "html",
    "htm": "html",
    "css": "css",
    "scss": "css",
    "less": "css",
}

# Directories that are never the user's code, by the same reasoning the server's ignore list
# uses. `.git` is required: the record lives inside it.
SKIP_DIRS = {".git", ".venv", "venv", "node_modules", "target", "dist", "build", "__pycache__",
             ".mypy_cache", ".pytest_cache", ".cache", "site-packages"}
SKIP_SUFFIX = (".min.js", ".min.css", ".lock", ".snap")


def candidate_files(repo, limit):
    """Source files in `repo`, biggest first: the interesting failures are in real code, and
    the limit should be spent on files that have enough in them to say something."""
    found = []
    for dirpath, dirnames, filenames in os.walk(repo):
        dirnames[:] = sorted(d for d in dirnames if d not in SKIP_DIRS and not d.startswith("."))
        for name in sorted(filenames):
            if name.endswith(SKIP_SUFFIX):
                continue
            ext = name.rsplit(".", 1)[-1].lower() if "." in name else ""
            lang = EXT_LANG.get(ext)
            if lang is None:
                continue
            path = os.path.join(dirpath, name)
            try:
                size = os.path.getsize(path)
            except OSError:
                continue
            found.append((size, path, lang))
    found.sort(reverse=True)
    return [(p, l) for _, p, l in found[:limit]] if limit else [(p, l) for _, p, l in found]


def trace_path(root):
    return os.path.join(root, ".git", "jev", "session.jsonl")


def analysis_entries(path, uri):
    """Every `analysis` entry for `uri` the record currently holds, oldest first."""
    out = []
    if not os.path.isfile(path):
        return out
    try:
        with open(path) as fh:
            for line in fh:
                try:
                    entry = json.loads(line)
                except Exception:
                    continue
                if entry.get("kind") == "analysis" and entry.get("uri") == uri:
                    out.append(entry)
    except OSError:
        return out
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True, help="the repository to walk")
    ap.add_argument("--bin", default=os.path.join(REPO, "target", "release", "jev-lsp"))
    ap.add_argument("--model-url", required=True, help="an OpenAI-compatible endpoint")
    ap.add_argument("--model", default=os.environ.get("JEV_MODEL"),
                    help="model name for every tier; defaults to JEV_MODEL, then the built-in")
    ap.add_argument("--limit", type=int, default=0, help="files to analyse, 0 for all")
    ap.add_argument("--timeout", type=float, default=60.0, help="seconds to wait per file")
    args = ap.parse_args()

    repo = os.path.abspath(args.repo)
    if not os.path.isdir(repo):
        print(f"repo_bench: no directory at {repo}", file=sys.stderr)
        return 2
    if not os.path.exists(args.bin):
        print(f"repo_bench: no binary at {args.bin}", file=sys.stderr)
        return 2

    files = candidate_files(repo, args.limit)
    if not files:
        print(f"repo_bench: no source files under {repo}", file=sys.stderr)
        return 2

    try:
        with urllib.request.urlopen(args.model_url.rstrip("/") + "/models", timeout=10):
            pass
    except Exception as e:  # noqa: BLE001 — any failure means the same thing to the caller
        print(f"repo_bench: {args.model_url} is not answering ({e}); "
              f"a stub would measure the stub", file=sys.stderr)
        return 2

    env = dict(os.environ, JEV_BASE_URL=args.model_url)
    if args.model:
        env["JEV_MODEL"] = args.model
        env["JEV_REVIEW_MODEL"] = args.model
    trace = trace_path(repo)

    print(f"repo: {repo}")
    print(f"model: {args.model or 'server default'} at {args.model_url}")
    print(f"files: {len(files)} (largest first)")
    print()

    server = Lsp([args.bin, "--stdio"], env)
    rows = []
    try:
        server.request("initialize", {
            "processId": os.getpid(),
            "rootUri": "file://" + repo,
            "capabilities": {"workspace": {"configuration": True, "workspaceFolders": True}},
        })
        server.notify("initialized", {})

        for path, lang in files:
            try:
                with open(path) as fh:
                    text = fh.read()
            except (OSError, UnicodeDecodeError) as e:
                rows.append((path, lang, None, 0, f"unreadable: {e}"))
                continue
            lines = text.count("\n") + 1
            uri = "file://" + path
            before = len(analysis_entries(trace, uri))
            server.notify("textDocument/didOpen", {"textDocument": {
                "uri": uri, "languageId": lang, "version": 1, "text": text}})
            server.notify("textDocument/didSave", {"textDocument": {"uri": uri}})

            deadline = time.time() + args.timeout
            entry = None
            while time.time() < deadline:
                found = analysis_entries(trace, uri)
                if len(found) > before:
                    entry = found[-1]
                    break
                time.sleep(0.2)
            if entry is None:
                rows.append((path, lang, None, lines, "no analysis within "
                             f"{args.timeout:.0f}s"))
            else:
                findings = entry.get("findings")
                if not isinstance(findings, list):
                    findings = [{}] * int(entry.get("count") or 0)
                rows.append((path, lang, findings, lines, None))
            server.notify("textDocument/didClose", {"textDocument": {"uri": uri}})
    finally:
        server.stop()

    rel = lambda p: os.path.relpath(p, repo)  # noqa: E731 — the report is relative to the repo
    width = max((len(rel(p)) for p, _, _, _, _ in rows), default=12)
    width = min(width, 68)
    print(f"{'file':<{width}} {'lines':>6} {'findings':>8}  detail")
    total_findings = 0
    total_lines = 0
    warnings = 0
    information = 0
    analysed = 0
    missed = 0
    for path, lang, findings, lines, note in rows:
        if findings is None:
            missed += 1
            print(f"{rel(path):<{width}} {lines:>6} {'-':>8}  {note}")
            continue
        analysed += 1
        total_findings += len(findings)
        total_lines += lines
        for f in findings:
            if f.get("severity") == "warning":
                warnings += 1
            elif f.get("severity") == "information":
                information += 1
        detail = "; ".join(f"L{f.get('line', '?')} {f.get('label', '')}" for f in findings)
        print(f"{rel(path):<{width}} {lines:>6} {len(findings):>8}  {detail[:110]}")

    print()
    print(f"files analysed     {analysed} of {len(files)}"
          + (f" ({missed} produced no analysis)" if missed else ""))
    print(f"findings           {total_findings} ({warnings} warning, {information} information)")
    print(f"lines              {total_lines}")
    if total_lines:
        print(f"per 1000 lines     {1000.0 * total_findings / total_lines:.2f}")
    cheap = Counter(os.path.splitext(p)[1] for p, _, f, _, _ in rows if f)
    print(f"by extension       " + (", ".join(f"{k or '(none)'} {v}" for k, v in
                                             cheap.most_common()) or "-"))
    print()
    print("A measurement, not a threshold. The number to watch is per 1000 lines: a change to")
    print("the prompt or the floor is an improvement when this falls and recall does not.")

    if analysed == 0:
        print("\nrepo_bench: nothing was analysed, so nothing was measured. Check that the "
              "endpoint serves `--model` and that the server is enabled.", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
