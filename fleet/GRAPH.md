# The agent graph

This file describes the work graph: the subagents that run one task. Section numbers start at 2
because other files cite them by number.

---

## 2. The work graph (dynamic, per task)

Nodes are **subagents** in `@FLEET@/.claude/agents/`. A new work graph is generated for each
task. The canonical shape is the **diamond**:

```
   goal
     │
     ▼
 ┌────────┐      read-only recon; writes facts into state
 │ scout  │
 └───┬────┘
     ▼
 ┌──────────┐    turns goal + facts into a plan AND into this graph's shape
 │architect │    ← this node is the router
 └───┬──────┘
     │  ── HUMAN GATE ──  you approve the plan before any code is written
     │
     ├──── fan-out (only if slices are genuinely independent) ────┐
     ▼                    ▼                    ▼                  │
 ┌────────┐          ┌────────┐          ┌────────┐               │
 │builder │          │builder │          │builder │   isolated    │
 │ slice1 │          │ slice2 │          │ slice3 │   worktrees   │
 └───┬────┘          └───┬────┘          └───┬────┘               │
     ▼                   ▼                   ▼                    │
 ┌────────┐          ┌────────┐          ┌────────┐               │
 │reviewer│          │reviewer│          │reviewer│   FRESH context each
 └───┬────┘          └───┬────┘          └───┬────┘   (never the builder's)
     │                   │                   │                    │
     │  conditional edge: REJECT ────────────┼────────────────────┘
     │                   │                   │        (max 2 loops, then escalate)
     └───────── fan-in ──┴───────────────────┘
                         ▼
                   ┌───────────┐   the single owned merge point
                   │integrator │
                   └─────┬─────┘
                         │  ── HUMAN GATE ── before deploy / migration / spend
                         ▼
                     ┌──────┐
                     │  ops │
                     └──────┘
```

### The stop rule (read this before building any graph)

Do **not** build a graph for:
- one file, one bug, one endpoint
- anything you could describe in a single paragraph
- anything where the slices would touch the same files

For those, use a **single loop**: one agent, one context, done. The graph costs real
tokens and real wall-clock. It earns that back only when slices are genuinely parallel
and verification genuinely needs a context that never saw the code being written.

Rough line: **3+ independent slices, or a change you would not merge unreviewed.**

**No repo, no diamond.** Before choosing a diamond, check that the target is a git repo:

```bash
git -C <target> rev-parse --is-inside-work-tree
```

If that exits non-zero, `isolation: "worktree"` is unexecutable — a worktree requires a
git repo — so the fan-out above has **no isolation and no rollback**. Builders write into
one tree and collide, `builders.<slice>.branch` is the empty string because there is no
branch, and a corrupted file cannot be restored because there is nothing to restore from.
Force **`single-loop`**, or make the target its own repo first. Full
pre-flight: `feature-graph` step 0.5.

One case forces `single-loop` even *with* a repo: a run that edits
`@FLEET@/.claude/agents/**` or `.claude/skills/**` is rewriting the definitions it
spawns from. Registration happens at session start, so start a fresh session before the
next fleet run.

### Delete fake edges

An edge exists only if a real artifact moves along it. If node B doesn't consume
anything node A produced, B doesn't depend on A — run them in parallel. Sequential
chains that exist only because they "feel ordered" are the single most common way
these graphs get slow.

---

## 3. Shared state — how edges actually carry data

Claude Code subagents start with a **fresh context** and return **only their final
text**. They cannot see each other's work. So "shared state travelling along edges" is
not a metaphor here — it is a file:

```
@FLEET@/.graph/runs/<run-id>/state.json      # relative to the project root
```

Contract for every node:
1. **On start:** read your brief — `python @FLEET@/.graph/brief.py --for
   <node>[:<slice>] <run-id>`. That is your primary inherited context: your goal, the
   plan entry that concerns you, and whatever a node in your position needs from
   whoever ran before you (scout's facts for a builder, a builder's report for its
   reviewer, the whole built/reviewed picture for the fan-in nodes). It is derived —
   never authored — from the same `state.json` this section has always described, so it
   cannot drift from the file and costs no node a token to keep in sync. Read
   `state.json` directly only for a key your brief omits that you actually need; do not
   default to the full file out of habit, because that is the token cost this brief
   exists to remove. `--for` degrades by naming what's missing rather than guessing —
   `.graph/brief.py`'s own docstring has the shape per node type.
2. **On finish:** append your result to your own key, including `written_by` naming
   yourself. Never rewrite another node's key, and never stamp `written_by` on one that
   is not yours.
3. **Return a HEADLINE, not the handoff.** Three lines, no verbatim command output, no
   file lists, no findings bodies. Rule 2 already put those in your key, which is what
   the next node actually reads — nothing consumes your return text but the orchestrator's
   main tab, and a human is the only thing there. Repeating the key in the return prints
   the machine channel into the human one, and the run then reads as several thousand
   words of correct, necessary, unreadable detail. Each node's own file gives its exact
   shape. `architect` and `ops` are the two exceptions: their returns are **gate**
   material, not status, and a human cannot approve a summary of a plan or a deploy they
   have not been shown.

`written_by` (added 2026-08-26) is the only authorship this file has ever carried.
Without it rule 2 was unverifiable *by construction*: an orchestrator hand-writing all
six keys read exactly like six nodes doing their jobs. `--audit` now rejects
`builders.<slice>` stamped `orchestrator`, and `reviews.<slice>` stamped `builder`.

**Checking that a node actually wrote its key:** `@FLEET@/.graph/verify-state.py`
takes a run id plus key names and exits 0 only if each key is present, non-empty, and not
still the `_schema.json` placeholder. It catches the one failure that matters most here —
a node that returned a summary without writing anything.

It does **not** check content, key shape, or staleness. The full list of what it misses
is in `feature-graph` § `verify-state.py`.

**The edges, however, are now machinery.** `verify-state.py --audit <run-id>` asks the
question a state file can answer without being told which node just ran: did the edges
hold? Builders written while `approved_by_human` is still false, a review with no build
behind it, a fan-in over a slice that never passed, a run closed `done` with a slice
unbuilt or not `PASS`, a key stamped with the wrong node's name, template text left
sitting inside an otherwise-written key. That
mode is fired automatically by `.claude/hooks/flag-state-gap.py` on every `Write`/`Edit`
of a `state.json` — including writes made by subagents, so the complaint lands on the
node that just wrote. It is silent on a merely half-filled run, because that is what work
in progress looks like.

So the split is: **ordering and authorship are enforced, content is not.** A node can
still write nonsense into its own key, on schedule, under its own name, and nothing will
notice.

### The heartbeat — what the nodes *did*

`state.json` records what each node **concluded**, and only when it finishes. Nothing in
it moves while a node is working, so a run in flight is invisible.

`.claude/hooks/record-activity.py` fills that in. On `SubagentStart`, `SubagentStop` and
every `PostToolUse` it appends one compact line to
`.graph/runs/<run-id>/activity.jsonl` — timestamp, event, `agent_type`, `agent_id`, tool
name. It is also the evidence base for four things
this fleet has never been able to answer with data rather than assertion:

- **model tiering** — §"Model tiering" argues cost hard and has never measured it. Tool
  counts and durations per `agent_type` are the measurement.
- **reviewer independence** — a re-review's `SubagentStart` carries a *new* `agent_id`,
  which makes "a fresh reviewer" checkable rather than asserted.
- **real parallelism** — when a diamond finally runs, overlapping timestamps are what
  prove the builders ran concurrently rather than merely being spawned together.
- **stalls** — the same tool repeating with no progress is visible in the tail.

**Note what this is not: an overseer agent.** One was considered and rejected. A subagent
is spawned, runs, returns text and ends — there is no loop for it to observe from, no
channel to its siblings, and a node consuming nothing and producing nothing is exactly
the fake edge §2 says to delete. The hook layer already runs alongside every node and is
handed `agent_id`/`agent_type` on every tool event. Oversight belongs where it can
actually see.

### The board — the human channel, rendered

Rule 3 caps what each node *says*. It does not by itself make a run legible: eight
headlines scattered between tool calls is still something you have to reassemble in your
head. `.graph/brief.py` does the reassembling.

```bash
python @FLEET@/.graph/brief.py            # the run .graph/CURRENT names
python @FLEET@/.graph/brief.py <run-id>
```

It prints one board: the gate, the goal, a line per node, a row per slice showing build
and verdict together, and — from `activity.jsonl` — what is running right now and for how
long. Roughly ten lines for a run whose `state.json` is several hundred.

**Nothing in it is authored.** Every value is derived from `state.json` and
`activity.jsonl`, which the fleet already writes. That is the design, not a shortcut: a
board no node writes cannot drift from the run, cannot be forged, and costs no node a
token. The moment a node is asked to author the summary as well, it can be wrong about it
— the same argument `--audit` makes against trusting a node's own account of its work.
It reuses `verify-state.py`'s placeholder rules rather than restating them, so "written"
means on the board exactly what it means to the audit.

**It also prints itself, at dispatch.** `.claude/hooks/show-board.py` fires on `PostToolUse`
for the `Agent` tool — the spawn call — and hands the board back as `systemMessage`,
**observed reaching the main tab 2026-09-03**. Not `SubagentStop`, which looks like the
right event and is not: it is a *display* event, so both its stdout and its `systemMessage`
go to Claude's context "instead of being shown in the transcript", reaching the orchestrator
and never the human.

**The hook fires when a node starts, not when it finishes** — measured, not assumed: all 32
`Agent` events in the heartbeat land within 0.2s of a `SubagentStart`, never near a node's
own stop. A spawn call returns a handle and the node runs in the background. So that board
shows the run as the node *picks it up*, and no hook can print one when the node comes back:
the event that fires then is `SubagentStop`, which cannot address a human.

So there are two boards per node, from two actors. The hook prints the dispatch board for
free. The orchestrator prints the **return** board itself, and the boards at the transitions
no node marks at all — after the gate, after a merge, at the close (`feature-graph` § the
board).

Schema in `@FLEET@/.graph/runs/_schema.json`. Because state is on disk, a run survives a
crashed session, a `/clear`, or you walking away — pick it back up by pointing a fresh
orchestrator at the run directory.

---

## 4. Node roster

| Node | Model | Context | Writes code? | Job |
|---|---|---|---|---|
| `scout` | haiku | read-only | no | Find the facts. What exists, where, what breaks. |
| `architect` | opus | read-only | no | Goal + facts → plan + graph shape. The router. |
| `builder` | sonnet | isolated worktree † | yes | Implement exactly one slice. |
| `reviewer` | opus | fresh, never the builder's | no | Adversarial. Has authority to REJECT. |
| `integrator` | opus | main tree | yes | The one owned merge point. Resolves conflicts. |
| `ops` | opus | main tree | yes | CI, env, deploy. Always behind a human gate. |

† **Only where the target is a git repo.** In degraded mode the builder runs in the main
tree with no isolation and no rollback, which is why that case is `single-loop` and one
builder. `builder.md`'s frontmatter says the same thing in the same words.

**Reviewer independence is non-negotiable.** A builder reviewing its own work is not a
verification edge, it's a fake edge. Always a separate agent invocation.


### Model tiering — why scout is cheap and the rest are not

The rule: **spend on judgment, not on retrieval.**

`scout` is the highest-token node in a typical run and the most mechanical one. It
globs, greps, reads, and reports `file:line`. That is pattern-matching, not reasoning —
haiku does it at roughly a fifth the cost of a mid-tier model, and it is usually the
single biggest lever on a run's total spend.

`architect`, `reviewer`, `integrator` and `ops` stay on opus, and two of them especially
must never be downgraded:

- **`reviewer`** — its entire value is catching what the builder missed. A verifier that
  misses the bug is *worse* than no verifier, because it launders a bad diff as reviewed.
  Cheapening this node doesn't save money, it removes the reason the graph exists.
- **`architect`** — it decides the shape, and shape errors are the expensive ones. A plan
  built on a wrong reading of the data ships broken.

**`builder` is sonnet** (changed 2026-09-09), and this is not the same argument as the
scout downgrade. A builder does not decide anything a human hasn't already approved — it
executes exactly one slice against a file set and a `done_when` a human signed off at the
gate (§5). The judgment already happened before the builder ever starts. What's left is
implementation against a fixed contract, and the reviewer — never downgraded, always a
fresh context — exists specifically to catch what a builder misses. That division of
labor is what makes tiering the builder safe: cheapen the checker and you remove the
reason the graph exists; cheapen the executor and the checker still does its job.

**The consequence of a cheap scout:** haiku is less able to self-scope, so its brief must
be tighter. Tell it exactly which questions to answer and which files to start from.
Don't hand it "go look at the app." A vague scout brief is where the savings evaporate —
it reads everything, returns mush, and the architect plans on sand.

**The mechanical half of that brief is now a script.** `@FLEET@/.graph/scout-facts.py`
computes what every scout was re-deriving by hand — git repo or not, branch, HEAD, dirty
state, commit identity, the stack observed on disk, and a `native:` line when it finds
native-app evidence — and `scout.md` step 0 runs it before anything else.

**Before reaching for a cheaper provider:** model tier is the second-biggest lever, not
the first. The stop rule is the first. One `single-loop` instead of an unjustified
six-node diamond saves more than downgrading every node in the fleet would.

Reference cost per MTok (in/out): haiku 4.5 $1/$5 · sonnet 5 $3/$15 · opus 5 $5/$25.

### Recursive reading — the same rule, applied inside one node

The same rule also applies *inside* a node. When a scout must read
an input too large to hold — a diamond's `activity.jsonl`
at 110KB — it does not read it. It loads it into `@FLEET@/.graph/rlm.py`, a
Recursive Language Model REPL (Zhang, Kraska & Khattab, arXiv:2512.24601): the input
sits in a variable, the node writes code to peek, grep and partition it, and maps haiku
sub-calls over the pieces. Only short output prefixes reach the node's own context. The
skill is `/rlm`.

```
 node (root) ──code──▶ rlm.py REPL ── context = <135KB file>
                          │  peek · grep · chunks          (free, exact)
                          └─ llm_map ──▶ claude -p haiku ×4 concurrent
                                          no tools, no settings, cwd outside the project
                          ◀── answers, combined in code, confirmed by grep
```

It is **retrieval moved to the cheap tier**, which is why its scope stops where this
section's protected nodes start: `reviewer`, `architect` and `integrator` never read
through it. A review done through haiku summaries is the downgrade the reviewer bullet
above forbids, done one level down. Sub-call output is a lead to confirm, never a FACT.
The scope and the measurements behind each default are in the `rlm.py` docstring.


---

## 5. Human gates

Place them exactly where a mistake gets expensive to undo:

- after `architect` — before any code exists (cheapest possible place to change your mind).
  **What you approve here is now binding**: `.claude/hooks/guard-builder-scope.py` denies
  a builder's `Write`/`Edit` outside `architect.plan[].files`, and denies a `Bash` command
  whose write target it can resolve outside them — a redirect, a heredoc, `tee`, `sed -i`,
  `rm`, `cp`, `curl -o`, a `python -c` body. So the file set is a boundary rather than a
  description. It is **mostly** a boundary on `Bash`: a write by a program the builder
  invokes (`pytest`, `npm run build`) is deliberately not caught, and a target that cannot
  be resolved from the command string is allowed with a warning rather than denied. Read
  the file list at the gate as a permission grant on three tools with a documented hole,
  not as a sandbox. Widening it after the fact is possible, deliberate and recorded —
  `scope_exceptions` plus the slice's `deviation_from_approved_plan` — never silent.
- before `ops` — deploys, DB migrations, anything that costs money or touches prod

Everywhere else, let it run.
