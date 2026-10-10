#!/usr/bin/env python
"""Decide whether a run may be closed, and prove the work actually landed.

    python @FLEET@/.graph/close-run.py            # the run .graph/CURRENT names
    python @FLEET@/.graph/close-run.py <run-id>
    python @FLEET@/.graph/close-run.py --recheck <run-id>   # audit a closed run

Exit 0 only when every blocker is clear. Exit 1 otherwise, naming each one.

**Why this exists.** An early run closed with its `log` reading
"5/5 slices PASS" while `reviews.s4` and `reviews.s5` still recorded the REJECT they had
already been fixed for, and `builders.closing_fix` had no reviewer at all. Nothing caught
it for a day, because nothing was looking. `feature-graph` answered that with a rule --
"before you set `status: done`, run `--audit` and get exit 0" -- placed at the exact
moment attention is lowest, when the work already feels finished. This is that rule with
a script behind it.

**The check nothing else makes.** `--audit` reads the state file, so it can only prove the
state file agrees with itself. A run whose reviews all say PASS and whose branch was never
merged passes the audit and is, in every way that matters, not done. So the merge is
verified against **git**, not against `state.json`: each slice's branch (or its recorded
commit, for a branch since deleted) must be an ancestor of the target repo's current HEAD.
That is the difference between a run that says it landed and a run that landed.

**It never writes.** Same contract as `verify-state.py`, and for the same reason: the
orchestrator owns `status` and `log`, and a script that closed the run on its behalf would
be the forgery `written_by` exists to catch, one key over. On green it prints the exact
close for the orchestrator to write itself.

Read-only, stdlib only, and it never raises on a partial run.
"""
import importlib.util
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
FLEET = os.path.dirname(HERE)
UMBRELLA = os.path.dirname(FLEET)
RUNS = os.path.join(HERE, "runs")
CURRENT = os.path.join(HERE, "CURRENT")
VERIFY = os.path.join(HERE, "verify-state.py")


def load_module(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def git(target, *args):
    """(exit code, stdout). Never raises; a missing git is just a non-zero code."""
    try:
        proc = subprocess.run(["git", "-C", target] + list(args),
                              capture_output=True, text=True)
    except OSError:
        return 1, ""
    return proc.returncode, proc.stdout.strip()


def target_repo(app):
    """The directory this run's work landed in: the project root, or None if no app is named."""
    if not app:
        return None
    return UMBRELLA


def merge_evidence(state, slice_id):
    """(kind, ref) -- the strongest proof this slice's work exists as a commit.

    A branch is the first choice. A branch deleted after merging is normal hygiene, so
    fall back to the commit the builder recorded; a past run wrote that field as
    "<sha> <subject>", hence the split.
    """
    branch = state.get("builders", {}).get(slice_id, {}).get("branch")
    if isinstance(branch, str) and branch.strip():
        # First token only. A branch name cannot contain whitespace, and builders have
        # written prose into this field -- one run's s1 records
        # "master (committed directly -- shape is single-loop...)", which as a whole
        # string resolves to nothing and reports an unprovable merge for work that
        # plainly landed.
        return "branch", branch.strip().split()[0]
    commit = state.get("builders", {}).get(slice_id, {}).get("commit")
    if isinstance(commit, str) and commit.strip():
        return "commit", commit.strip().split()[0]
    return None, None


def check(run_id, recheck=False):
    """(blockers, warnings, notes). Blockers are what stop a close."""
    verify = load_module(VERIFY, "verify_state")
    template = verify.load_template()
    state, path = verify.load(run_id)
    if not isinstance(state, dict):
        return ["%s is not a JSON object" % path], [], []

    blockers, warnings, notes = [], [], []

    status = str(verify.resolve(state, "status")[1] or "").strip().lower()
    if status == "done" and not recheck:
        blockers.append("run is already `status: done` -- nothing to close. "
                        "Use --recheck to audit it anyway")
    notes.append("status: %s" % (status or "?"))

    if verify.resolve(state, "approved_by_human")[1] is not True:
        blockers.append("approved_by_human is not true -- a run that never passed the "
                        "human gate cannot be closed as done")

    # -- the audit. It proves the state file agrees with itself, and nothing more.
    for problem in verify.audit(state, template):
        blockers.append("audit: %s" % problem)

    # -- every slice built and PASSed. `real_slices` includes off-plan ids, which is the
    #    point: `builders.closing_fix` in an early run was real work with no reviewer.
    #    "PASSed" is the LATEST review attempt, not the top of `reviews.<slice>`: a
    #    re-review nests as `attempt_N` and attempt 1's REJECT stays where its reviewer
    #    wrote it, so reading the top level certified a fixed slice as failed and is why
    #    an early run could not close. The rule is imported from
    #    verify-state.py rather than restated here -- a second copy of it is a second
    #    chance to disagree, which is the defect, not the fix.
    slices = verify.real_slices(state, template)
    if not slices:
        warnings.append("this run has no slices -- nothing to verify as built")
    planned = set()
    _, plan = verify.resolve(state, "architect.plan")
    if isinstance(plan, list):
        planned = {e["slice"] for e in plan
                   if isinstance(e, dict) and isinstance(e.get("slice"), str)}

    for sid in slices:
        built = not verify.unwritten(state, template, "builders.%s" % sid)
        reviewed = not verify.unwritten(state, template, "reviews.%s" % sid)
        review = verify.resolve(state, "reviews.%s" % sid)[1]
        verdict = verify.slice_verdict(state, sid)
        where = "%s%s" % (sid, "" if sid in planned else " (off-plan)")
        if not built:
            blockers.append("slice %s was never built" % where)
        elif not reviewed:
            blockers.append("slice %s has no review -- it was never independently checked"
                            % where)
        elif verdict != "PASS":
            blockers.append("slice %s is %s, not PASS" % (where, verdict or "unwritten"))
        elif verify.ever_rejected(review):
            # It closes -- and it closes as what it was. A slice that was REJECTed and
            # then fixed is history worth keeping, so the close report says it looped
            # rather than rendering a clean first-try PASS. `ever_rejected` is display
            # only and must never gate anything; it is a note, never a blocker.
            notes.append("slice %s: PASS on attempt %s, after an earlier REJECT -- the "
                         "loop is on the record" % (where, verify.final_verdict(review)["attempt"]))
        build_status = str(verify.resolve(state, "builders.%s.status" % sid)[1] or "").strip()
        if built and build_status and build_status.lower() not in ("done",):
            blockers.append("slice %s reports builder status %r" % (where, build_status))

    # -- the merge, proved against git rather than against the state file.
    app = verify.resolve(state, "app")[1]
    target = target_repo(app)
    if target is None or not os.path.isdir(target):
        warnings.append("cannot resolve a directory for app %r -- the merge is unproven"
                        % app)
    elif git(target, "rev-parse", "--is-inside-work-tree")[0] != 0:
        notes.append("%s is not a git repo -- degraded mode, there is nothing to merge"
                     % os.path.relpath(target, UMBRELLA).replace("\\", "/"))
    else:
        code, head = git(target, "symbolic-ref", "--short", "HEAD")
        head = head if code == 0 and head else "HEAD"
        notes.append("merge target: %s @ %s"
                     % (os.path.relpath(target, UMBRELLA).replace("\\", "/"), head))
        for sid in slices:
            if verify.unwritten(state, template, "builders.%s" % sid):
                continue          # never built; its own blocker is already recorded
            kind, ref = merge_evidence(state, sid)
            if kind is None:
                notes.append("slice %s records no branch -- committed in place, nothing "
                             "to merge" % sid)
                continue
            if git(target, "rev-parse", "--verify", "--quiet", ref + "^{commit}")[0] != 0:
                warnings.append("slice %s names %s %s, which does not resolve in %s -- "
                                "the merge cannot be proved from refs"
                                % (sid, kind, ref, head))
                continue
            if kind == "branch" and ref == head:
                notes.append("slice %s committed directly to %s -- degraded mode, no "
                             "merge to prove" % (sid, head))
                continue
            if git(target, "merge-base", "--is-ancestor", ref, "HEAD")[0] != 0:
                blockers.append("slice %s's %s %s is NOT an ancestor of %s -- the work "
                                "was reviewed but never landed"
                                % (sid, kind, ref, head))
            else:
                notes.append("slice %s: %s %s is in %s" % (sid, kind, ref, head))

    return blockers, warnings, notes


def pointer():
    """The run id `.graph/CURRENT` names, or None. Never raises."""
    try:
        with open(CURRENT, encoding="utf-8") as fh:
            return fh.read().strip() or None
    except OSError:
        return None


def main(argv):
    recheck = "--recheck" in argv
    argv = [a for a in argv if a != "--recheck"]
    run_id = argv[0] if argv else pointer()
    if not run_id:
        sys.stderr.write("close-run: no run given, and .graph/CURRENT names none\n")
        return 1

    try:
        blockers, warnings, notes = check(run_id, recheck)
    except SystemExit:
        raise
    except Exception as exc:
        sys.stderr.write("close-run: cannot check %s (%s)\n" % (run_id, exc))
        return 1

    for line in notes:
        print("  - %s" % line)
    sys.stdout.flush()          # or the evidence lands after the blockers that cite it
    for line in warnings:
        sys.stderr.write("close-run: WARNING: %s\n" % line)
    if blockers:
        for line in blockers:
            sys.stderr.write("close-run: BLOCKER: %s\n" % line)
        sys.stderr.write("close-run: %s -- NOT closeable, %d blocker(s)\n"
                         % (run_id, len(blockers)))
        return 1

    print("close-run: %s -- closeable%s" % (
        run_id, " (%d warning(s) above)" % len(warnings) if warnings else ""))
    print("  write it yourself, as the orchestrator: set `status` to \"done\" and append "
          "one `log` entry saying what closed and when.")
    if pointer() == run_id:
        # Not a blocker, and not something this script does for you -- it never writes,
        # and `.graph/CURRENT` is no exception. But nothing else in the fleet clears the
        # pointer, so it survives the close and names a finished run until the next run
        # opens. Three readers already check the status behind it and go quiet
        # (`guard-builder-scope.py`, `record-activity.py`, `show-board.py`), which is why
        # this is hygiene rather than a defect: the cost of leaving it is that every one
        # of them has to keep being right about it.
        print("  then clear `.graph/CURRENT` -- it still names this run, and the close "
              "does not empty it.")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
