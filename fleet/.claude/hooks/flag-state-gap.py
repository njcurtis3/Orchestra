#!/usr/bin/env python
"""PostToolUse hook: audit a run's state.json for edge violations the moment it changes.

This is what "fire verify-state.py from a hook" can actually mean. The script's original
mode takes a run id plus the KEY a node was supposed to write, and a hook has no way to
know which node just finished -- it sees a file path. So this fires the script's
`--audit` half instead, which asks the question a lone state.json can answer: given
everything written so far, did the graph's edges hold?

What that turns from convention into machinery:
  - builders written with `approved_by_human` still false -- the human gate skipped
  - a review with no build behind it
  - a fan-in over a slice that never passed
  - a run closed `done` with a slice unbuilt or not PASSed
  - template text left sitting inside an otherwise-written key (blind spot 6)

Deliberately SILENT on a merely half-filled run. Mid-run, most keys are unwritten and
that is exactly what work in progress looks like; a hook that complained on every
intermediate write would be switched off inside a day, and then it checks nothing.

The rule lives in `verify-state.py`, not here.
`importlib` rather than `import` because the filename is
hyphenated and not a legal module name.

The `.claude` directory may be a junction or symlink, so resolve the path before
routing on it, or the hook is silent on half the paths agents actually use.

Exit 0 always -- a hook must never block a legitimate edit.
"""
import importlib.util
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
VERIFY = os.path.normpath(os.path.join(HERE, "..", "..", ".graph", "verify-state.py"))


def _resolve(path):
    """Absolute, forward-slashed, junction/symlink-resolved. Never raises."""
    try:
        real = os.path.realpath(path)
    except Exception:
        real = path
    if not os.path.isabs(str(real)):
        try:
            real = os.path.abspath(real)
        except Exception:
            pass
    return str(real).replace("\\", "/")


def _load_verifier():
    spec = importlib.util.spec_from_file_location("verify_state", VERIFY)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main():
    try:
        payload = json.load(sys.stdin)
    except Exception:
        return

    ti = payload.get("tool_input") or {}
    tr = payload.get("tool_response") or {}
    path = ti.get("file_path") or (tr.get("filePath") if isinstance(tr, dict) else None)
    if not path:
        return

    norm = _resolve(path)
    if "/.graph/runs/" not in norm:
        return
    if os.path.basename(norm) != "state.json":   # _schema.json is a definition, not a run
        return

    run = os.path.basename(os.path.dirname(norm))

    try:
        with open(norm, encoding="utf-8") as fh:
            state = json.load(fh)
    except ValueError as exc:
        # A run whose state file will not parse has stopped being a run: every node
        # downstream reads this on start and gets nothing.
        _emit(run, ["the file is not valid JSON (%s) -- every node downstream reads "
                    "this on start and will get nothing" % exc])
        return
    except OSError:
        return

    if not isinstance(state, dict):
        _emit(run, ["the file is not a JSON object"])
        return

    try:
        verifier = _load_verifier()
        problems = verifier.audit(state, verifier.load_template())
    except Exception:
        return      # a broken checker must never break an edit

    if problems:
        _emit(run, problems)


def _emit(run, problems):
    listed = "\n".join("  - %s" % p for p in problems)
    json.dump({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": (
                "[state-audit] `%s` now has %d edge violation(s):\n%s\n"
                "These are violations of the work graph itself (`GRAPH.md` §3, "
                "`feature-graph` steps 4–6), not style. Fix the state file or the "
                "sequence before advancing the run. If a node other than you owns the "
                "offending key, re-prompt that node to write it — do not write "
                "another node's key for it. Re-check with "
                "`python @FLEET@/.graph/verify-state.py --audit %s`."
                % (run, len(problems), listed, run)
            ),
        }
    }, sys.stdout)


main()
