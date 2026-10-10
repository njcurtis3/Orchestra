---
name: feature-graph
description: Run a task through the work graph — scout, architect, human gate, then either a single loop or a parallel diamond of builders and independent reviewers converging on one integrator. Use when starting any non-trivial piece of work on an app. Also use when asked to "run the graph" or "fan this out".
---

# feature-graph

You are the **orchestrator**. You do not implement. You own the state file and the edges.

Run this from your project root. The fleet's files live under `@FLEET@/`.

## Step 0 — the stop rule

Before anything: is this actually a graph? One file, one bug, one endpoint, or something
you could describe in a paragraph is a **single loop**. Say so, do it directly, and skip
this skill. Graphs cost tokens and wall-clock; make them earn it.

## Step 0.5 — pre-flight: can this target hold a diamond?

Two checks. Both run **before** you pick a shape, and neither is optional. This step is
numbered `0.5` on purpose: nothing downstream may renumber.

**1. Is the target a git repo?**

```bash
python @FLEET@/.graph/scout-facts.py             # answers this and more
git -C <target> rev-parse --is-inside-work-tree        # or the bare check
```

The collector prints `NOT A GIT REPO -> diamond forced to single-loop` when it applies,
along with the branch, HEAD and per-repo commit identity you will want anyway. Compute
this every run — never carry it over from a previous one.

A worktree requires a git repo. If that exits non-zero there is **no isolation and no
rollback** — parallel builders write into one tree and collide (`GRAPH.md` § the stop
rule), and a bad edit cannot be reverted because there is nothing to revert to. Two
allowed responses, never a third:

- run **`single-loop`** regardless of slice count, and say in the final summary that the
  shape was *forced* by the absence of a repo rather than chosen; or
- make the target a repo first (`git init` in that directory alone, one baseline commit),
  then the diamond is on the table again.

A run whose target is any directory that is not itself a repo is always in degraded mode.

**What degraded mode costs, stated plainly:** no `isolation: "worktree"`, so no parallel
builders. No branch per slice, so `builders.<slice>.branch` is the empty string. No
`git checkout --` and no `reset --hard`, so a builder that corrupts a file has destroyed
it. The blast radius of one bad edit is the whole target.

**2. Is the target the fleet's own definitions?**

If the run edits `@FLEET@/.claude/agents/**` or `@FLEET@/.claude/skills/**`,
force **`single-loop`** regardless of slice count and regardless of the repo check. You
are rewriting the definitions you spawn from. Tell the user to **start a fresh session
before the next fleet run**: agent registration happens at session start, so what is
running now is what was on disk when the session opened — not necessarily what is on
disk after the run.

## Step 1 — open the run

```bash
RUN=$(date +%Y-%m-%d)-<short-slug>
mkdir -p @FLEET@/.graph/runs/$RUN
cp @FLEET@/.graph/runs/_schema.json @FLEET@/.graph/runs/$RUN/state.json
printf '%s' "$RUN" > @FLEET@/.graph/CURRENT
```

`.graph/CURRENT` names the open run. Hooks fire with a file path and an `agent_type` and
are never told which run they are in, so without this pointer nothing outside the
orchestrator can find the run's state — it is what makes `guard-builder-scope.py` (step
5) possible at all. It is untracked and disposable: a stale pointer to a closed run is
ignored, so there is nothing to clean up.

Fill `run_id`, `goal` (the user's own words), and `app` (the project's name).
If the goal spans two apps, it is two runs. Split it.

## Step 2 — scout

Spawn `scout` with the run path. It appends verified facts. Read them yourself before
continuing — you are the one who has to notice if the facts kill the goal.

Do not advance until this exits 0 — a node that returned a summary without writing
its key has not run:

```bash
python @FLEET@/.graph/verify-state.py $RUN scout
```

**Write the brief tightly.** `scout` runs on haiku (see GRAPH.md § Model tiering), which
is cheap but does not self-scope well. Hand it:

- the project and the two entry docs to start from
- a numbered list of the specific questions it must answer
- your *unverified guess* at which files are involved, explicitly marked as a guess to
  confirm or correct
- one thing you suspect will break the plan, named — so it hunts rather than surveys

"Go look at the app" produces mush and the architect then plans on sand. That is where a
cheap scout stops being cheap.

## Step 3 — architect

Spawn `architect` with the run path. It returns a shape and slices.

```bash
python @FLEET@/.graph/verify-state.py $RUN architect
```

## Step 4 — HUMAN GATE ⛔

Show the user the shape, the slices, the edges, and the NOT DOING list. Wait.
This is the cheapest possible moment to change direction — no code exists yet. Never
skip it because the plan "looks obviously right."

Set `approved_by_human: true` only after they actually say so.

## Step 5 — execute

**The approved file set is now a boundary, not a description.**
`.claude/hooks/guard-builder-scope.py` is a `PreToolUse` hook that **denies** a
`Write`/`Edit` — and since 2026-09-09 a `Bash` write — from any `builder` to a path outside
the union of `architect.plan[].files` (plus the run's own `state.json`).
For `Bash` the command string is classified for write targets (redirect, heredoc, `tee`,
`sed -i`, `rm`, `cp`, `curl -o`, an interpreter body) and each resolved path is judged the
same way. It reads the run through `.graph/CURRENT`, so step 1's pointer is load-bearing
here.

**Read the `Bash` half as mostly guarded, not guarded.** What the guard cannot see it
allows: a write performed by a program the builder invokes (`npm run build`, `pytest`, a
script written to an approved path and then run) is out of scope by design, since catching
it means denying every test command. A write shape whose target cannot be resolved from
the string — a shell variable, a `$(...)`, a command over the classifier's 128KB cap —
is allowed and **recorded as a warning**, which closes the trace half of the hole where it
cannot close the denial half. Plan on that: the gate's file set is a permission grant
enforced on three tools, not a sandbox.

When a builder comes back saying it was denied, that is the gate working. Decide, do not
reflex-widen:

- the work belongs to **another slice** — send it there; or
- the **approved file set was wrong**. Then extend it deliberately: append the path to
  `scope_exceptions` in `state.json` (you own that key), and require the builder to record
  `deviation_from_approved_plan` on its slice. Do **not** edit `architect.plan` to widen
  the set — that is rewriting another node's key, and `--audit` now catches it.

**If `single-loop`:** spawn one `builder`, then one `reviewer`.

**Then merge it — this step is yours.** A single-loop run still leaves a branch behind. On `PASS`, and only on `PASS`,
the orchestrator merges the one reviewed branch itself.

```bash
git -C <target> merge --no-ff <branch-from-builders.<slice>.branch>
```

If the merge **conflicts, stop.** Do not resolve it. Spawn `integrator`, which owns conflict
resolution, and let it write the `integrator` key. A clean merge of already-reviewed work is
bookkeeping; a conflict is a judgment call about code, and that is a different node's job.

In degraded mode (no repo, or the builder committed directly to the main branch —
`builders.<slice>.branch` is an empty string) there is nothing to merge. Skip this and say
so in the final summary.

Record the merge commit in `log`. No integrator otherwise.

**No repo, no diamond.** Re-run the step 0.5 check before you fan out:

```bash
git -C <target> rev-parse --is-inside-work-tree
```

If it exits non-zero, `isolation: "worktree"` is **unexecutable** — a worktree requires a
git repo. Do not fan out. Run the single-loop branch above, sequentially, and state in the
final summary that the shape was forced by the missing repo. Never fan out without
isolation and hope the file sets stay disjoint; that is how two builders overwrite each
other with no way back.

**If `diamond`:** spawn **all builders in one message** so they run concurrently, each with
`isolation: "worktree"` (they edit files in parallel; without isolation they will collide).

As each builder returns, immediately spawn its `reviewer` — do **not** wait for the other
builders. That barrier is the most common way these runs waste wall-clock. A slice that
finished in 2 minutes should be under review while a slow slice is still building.

**Brief the reviewer to the slice's `risk` tag — this is a token lever, not a formality.**
`risk: high` (real/sensitive data, a shared file, or logic a test can't see through) gets
the full adversarial brief: re-derive everything from scratch, do not trust the builder's
self-report, re-walk by hand what the test cannot check. `risk: low` gets a lighter
brief: re-run `done_when` on the branch alone, confirm the diff stays inside the approved
file set, read the diff once for correctness. Do not spend adversarial-depth review on a
slice the architect tagged `low` — that is the exact overhead this tagging exists to cut.
If a `low`-tagged reviewer finds something that makes it doubt the tag, it re-tags the
slice `high` in its own `reviews.<slice>` note and reviews accordingly; it does not silently
apply high-effort review without saying why the architect's call was wrong.

After each builder returns, and again after each reviewer, verify the key landed
before moving on:

```bash
python @FLEET@/.graph/verify-state.py $RUN builders.<slice>
python @FLEET@/.graph/verify-state.py $RUN reviews.<slice>
```

An empty key exits 1. Re-prompt that node to write it; do not write it for them.

On `REJECT`: send the findings back to that slice's builder. Re-review with a **fresh**
reviewer. Max 2 attempts, then stop and escalate to the user.

## Step 6 — fan-in

**Diamond only** — single-loop lands its own branch in step 5 and skips this.

When every slice is `PASS`, spawn `integrator` once. It merges and runs the **full** suite.
If it comes back `blocked`, report which merge turned it red — do not paper over it.

```bash
python @FLEET@/.graph/verify-state.py $RUN integrator
```

## Step 7 — ops

Only if the user asks. Behind its own gate.

## `verify-state.py` — what it checks, and what it does not

`@FLEET@/.graph/verify-state.py` is a read-only, stdlib-only checker. Give it a run
id (or a direct path to a `state.json`) plus one or more key names — dotted keys like
`builders.s2` work — and it exits 0 only if every named key is **present**, **non-empty**,
and **not still the `_schema.json` placeholder**. On failure it exits 1 and prints one
line naming the key. It never writes.

That is the whole of the named-key mode, and **it is a check, not a gate** — it runs
because the orchestrator chooses to run it. It has to be that way: it needs to be told
which key to look at, and only the orchestrator knows which node just returned.

### `--audit` — the half that fires itself

```bash
python @FLEET@/.graph/verify-state.py --audit $RUN
```

Added 2026-08-26. Takes no key, because it asks a different question: given everything
written so far, **did the edges hold?** It reports only violations —

- `builders.<slice>` written while `approved_by_human` is not `true` (step 4 skipped)
- `reviews.<slice>` written with no `builders.<slice>` behind it
- `integrator` written while any slice is `REJECT` or unreviewed (step 6 jumped)
- `ops.actions` non-empty with no approval (step 7's gate skipped)
- `status: done` with a planned slice unbuilt, or a review that is not `PASS`
- a key whose `written_by` names the wrong node — `builders.<slice>` stamped
  `orchestrator`, `reviews.<slice>` stamped `builder` (blind spot 3 below)
- template text still sitting inside an otherwise-written key (blind spot 6 below)

`.claude/hooks/flag-state-gap.py` fires this on every `Write`/`Edit` of any
`state.json`, subagent writes included, so a node that breaks an edge is told
immediately rather than at the next time someone remembers to check. You can still run
it by hand; you mostly will not need to.

It is silent on a half-filled run — mid-run, most keys are unwritten and that is what
work in progress looks like. It is **not** a substitute for the named-key calls above:
those catch a node that returned a summary without writing anything, which mid-run
`--audit` deliberately says nothing about.

Seven things the named-key mode does **not** check. Do not oversell it — and note that
`--audit` now covers (3) and (6) and fires itself, so (7) is no longer true of the script
as a whole. **(1), (2), (4) and (5) are still open**: ordering and authorship are
machinery now, content is not.

1. **Content correctness.** A key holding `{"status": "done"}` and nothing else passes.
2. **Key shape.** It consults `_schema.json` for placeholder *identity* only, never for
   structure — a missing `branch`, a missing `verdict`, or an invented field all pass.
3. **Who wrote the key.** — **closed 2026-08-26.** Every node key now carries
   `written_by`, each node stamps its own, and `--audit` rejects a key naming any other
   node. **This is a constraint on you**: hand-writing `builders.s1` yourself and
   stamping it `builder` is forgery, and writing it honestly as `orchestrator` fails the
   audit. Re-prompt the node instead. Runs opened before 2026-08-26 carry no stamps at
   all and report exactly one line saying so; they are not retro-fitted.
4. **Staleness.** No timestamps, so a key left over from a previous attempt passes.
5. **Numbers and booleans are non-empty by design** — a real `parallel_safe: false` must
   not look unwritten — so a hand-written `{"gated": false}` that differs from the
   template passes.
6. **Partial placeholders.** Identity is compared on the whole value, so a
   `builders.<slice>` whose `status` was filled in while `branch` is still the template's
   description passes. — **`--audit` catches this**, on string leaves.
7. ~~**It is advisory, not enforcing.**~~ — **superseded 2026-08-26.** `--audit` fires
   from `flag-state-gap.py` on every `state.json` write. Ordering is now enforced;
   **content still is not**, and (1)–(5) above are untouched by it. A node that writes
   confident nonsense into its own key on schedule still sails through both modes.

## The board — what you print between nodes

`verify-state.py` tells *you* that a key landed. `brief.py` is what you show the **human**:

```bash
python @FLEET@/.graph/brief.py $RUN
```

One board, about ten lines: the gate, the goal, a line per node, a row per slice with its
build and its verdict side by side, and — from `activity.jsonl` — what is running right
now and for how long.

**Dispatching a node prints one for you.** `.claude/hooks/show-board.py` fires on the
`Agent` tool's `PostToolUse` and renders the board as `systemMessage`. Measured 2026-09-03:
that event lands within 0.2s of the node **starting**, not finishing — a spawn call returns
a handle and the node runs in the background. So the free board is the one that shows the
run as the node picks it up. Do not print another one right after you spawn.

**You print the board when a node comes back.** No hook can: the event that fires at a
node's return is `SubagentStop`, and its output goes to your context instead of the
transcript, so it reaches you and never the human. This is the board that matters most —
it is the one carrying what the node just did — and it exists only if you run it:

    python @FLEET@/.graph/brief.py $RUN

Run it yourself after every node return, and at the transitions no node marks at all: after
the human gate, after a merge you performed, and at the close.

**Do not paste a node's return block into the main tab, and do not re-narrate it in
prose.** Since 2026-09-03 every node's return is already a headline (`GRAPH.md` § 3,
rule 3) — relaying it doubles that headline, and summarizing it puts a third account of
the same work on screen. That is exactly how the main tab became unreadable: not one
verbose node, but the same run reported three times at three altitudes. The detail is
never lost. It is in the node's own tab, and in its key, and `brief.py`'s last line names
the file.

The two exceptions are the two gates. At **step 4** you show the architect's plan in full,
and at **step 7** the ops proposal in full. Those are decisions, not status, and nobody
can approve a headline.

## Orchestrator rules

- **Never implement anything yourself.** The moment you edit a file you have destroyed the
  independence the graph is built on. **Merging an already-reviewed branch is not
  implementing** — it writes no code and makes no choice, and step 5 assigns it to you.
  *Resolving a conflict is*, and that is `integrator`'s job, not yours.
- **Never review a slice yourself.** You have seen the builder's summary; your context is
  contaminated. Always a fresh `reviewer`.
- **Never write another node's key, and the audit can now tell.** If a node returned
  without writing, re-prompt *it*. Filling the key in yourself and stamping it with that
  node's name is forgery, not bookkeeping.
- Append to `log` in state at every transition. That log is how a fresh session resumes.
- **Let the board speak; do not relay the node.** The `Agent` hook prints one when you
  *dispatch* a node. Run `python @FLEET@/.graph/brief.py $RUN` yourself when a node
  **returns** — no hook can do that one — and at the transitions no node marks at all: the
  gate, a merge, the close. See § the board.
- **Do not close the run by hand. Use `/close-run`.** It runs `--audit`, checks every
  slice — off-plan ones included — was built and `PASS`ed, and then proves in **git** that
  the work actually merged, which no reading of `state.json` can do. Only when it exits 0
  do you write `status: done` and the closing `log` entry yourself.
- Report faithfully. If a slice was skipped, a test failed, or you dropped scope, say so
  explicitly in the final summary.

## Resuming

Point a fresh session at `@FLEET@/.graph/runs/<run-id>/state.json`. The state file is the run —
context loss is not run loss.
