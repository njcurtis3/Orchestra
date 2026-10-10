---
name: close-run
description: Close an open work-graph run — prove every slice was built, reviewed PASS and actually merged into git, then write the close. Use when a run's work is finished, when asked to "close the run", "finish this run" or "mark it done", and before setting any run's status to done.
---

# close-run

You are the **orchestrator**. This is the last thing you do to a run, and it is the step
this fleet can most easily get wrong: the close is where attention is lowest and the record is most likely to be wrong, so it gets a script.

## Step 1 — check

```bash
python @FLEET@/.graph/close-run.py            # the run .graph/CURRENT names
python @FLEET@/.graph/close-run.py <run-id>
```

Exit 0 means closeable. Exit 1 prints every blocker and stops.

It asks four things, and the fourth is the one nothing else in the fleet asks:

1. `verify-state.py --audit` is clean — the state file agrees with itself
2. the human gate was passed
3. every slice, **including off-plan ones**, was built, reviewed, and `PASS`
4. **the work is in git.** Each slice's branch — or its recorded commit, for a branch
   since deleted — must be an ancestor of the target repo's HEAD

Check 4 exists because the first three read `state.json`, and a state file cannot know
whether a merge happened. A run whose slices all say PASS and whose branch was never
merged passes the audit and is, in every way that matters, not done.

Degraded shapes are **notes, not blockers**: a target that is not a git repo, a builder
that committed straight to the main branch, a slice with no branch at all. Those are what
`feature-graph` step 0.5 describes, not failures.

## Step 2 — on blockers, stop

Do not paper over one. Each maps to a real action:

| Blocker | What it means |
|---|---|
| slice is `REJECT` | the builder never fixed it, or the re-review never landed its verdict |
| slice has no review | spawn a fresh `reviewer` — never review it yourself |
| slice `(off-plan)` | real work nobody approved or checked. Review it or revert it |
| branch is NOT an ancestor | the merge never happened. Go do `feature-graph` step 5 or 6 |
| `approved_by_human` not true | the gate was skipped, or recorded wrongly |

A run that cannot close is not a run to close quietly. If the work is genuinely abandoned,
`parked` or `blocked` is the honest status.

## Step 3 — write the close yourself

`close-run.py` **never writes**, for the same reason `verify-state.py` never writes: you
own `status` and `log`, and a script closing the run on your behalf would be the forgery
`written_by` exists to catch, one key over.

On green, set `status` to `"done"` and append **one** `log` entry naming what closed, the
merge commit if there was one, and anything you dropped. Then **clear `.graph/CURRENT`** —
nothing else does, so it goes on naming a finished run until the next one opens. The three
hooks that read it check the status behind it and go quiet on a closed run, so a stale
pointer is not a live hazard; it is a fact about the fleet that is no longer true, and
leaving it means all three have to keep being right about it forever. Then print the final
board:

```bash
python @FLEET@/.graph/brief.py <run-id>
```

The board goes quiet after this — `brief.py` shows no live lane for a closed run, and
`show-board.py` stops emitting one. That silence is the close taking effect.

## What this skill is not

- **Not a substitute for the reviewer.** It checks that a verdict exists and says PASS. It
  has no opinion on whether the review was any good.
- **Not a merge.** If check 4 fails, the fix is `feature-graph` step 5 or 6, not this skill.
  A clean merge is yours; a conflict is `integrator`'s.
- **Not for `ops`.** Deploying is step 7, behind its own gate, and a closed run does not
  authorize it.

To re-examine a run that is already closed — checking whether an old run
would pass today — use `--recheck`.
