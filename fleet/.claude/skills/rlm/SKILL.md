---
name: rlm
description: Read an input too large to hold in context — a long file, a run's activity.jsonl, a big log — as a Recursive Language Model, by loading it into a REPL and mapping cheap haiku sub-calls over pieces of it, instead of reading it whole. Use when a scout or audit would otherwise read more than ~50KB, or when asked to "RLM this", "map over this file" or "read this without loading it".
---

# rlm

Zhang, Kraska & Khattab, *Recursive Language Models* (arXiv:2512.24601). You — the node
already running — are the **root** model. The long input never enters your context. It
sits in a REPL as `context`; you write code to inspect and split it, and a cheaper
**sub-model** (haiku, no tools, no fleet settings) reads the pieces. Only a short prefix of
each cell's output comes back to you.

The tool is `@FLEET@/.graph/rlm.py`. Its docstring carries the measurements behind
every default below; read it before changing one.

## When — and when not

**Use it** when the thing you must read is over ~50KB, or when the question is the kind
that needs *every* part of a large input looked at (an inventory, a count, "which of these
are still open"). Those are the tasks where reading it whole either does not fit or rots.

**Do not use it:**

- **Under ~50KB.** Read the file. A sub-call is ~$0.01–0.04 and 10–60s; it buys nothing
  on input that fits comfortably.
- **For a needle you can grep for.** `grep` inside the REPL is free and exact. A sub-call
  is for *semantic* questions over a piece, not for finding a string.
- **As `reviewer`, `architect` or `integrator`.** Never. RLM hands the reading to haiku,
  and those are the nodes `GRAPH.md` § Model tiering says are never downgraded. A review
  done through haiku summaries is a laundered review.
- **From a hook.** A sub-call is a network call.

## The protocol

```bash
R=@FLEET@/.graph/rlm.py
python $R load <session> <file> [--budget 0.50] [--effort low]   # 1. load
python $R exec <session> <<'PY'                                  # 2..n. cells
print(len(context), context.count("\n"))
print(peek(0, 1500))
PY
python $R final <session>                                        # the answer
python $R cost  <session>                                        # what it cost
```

Name the session after the run and the node: `<run-id>-scout`. One input per session.

**1. Peek before you plan.** Length, line count, the first ~1500 chars, the headings
(`grep(r"^#+ ")`). The paper's finding is that the *first decomposition* decides the
outcome on dense inputs — choose it from structure you have seen, not guessed.

**2. Narrow for free.** `grep(pattern, window=2)` returns `(line_no, text)` — 1-based, so
it is already a `file:line`. Cut the input down with code before spending anything.

**3. Map over what is left.** `chunks(30000)` splits on line boundaries and returns
`(start_line, text)`. `llm_map(question, pieces)` runs the sub-calls concurrently (4 at a
time) and returns answers in input order. Ask each piece a question with a **checkable,
structured answer** — `'#N' per line`, `file:line — claim`, `NONE` — so you can combine
them with code rather than by reading them.

**4. Combine in code, verify against `context`.** Parse the answers, de-duplicate, then
check each claim in the source before you rely on it. **A sub-model answer is a lead, not
a fact.** Sub-models add items the source never had; one `grep` against `context` catches
that. For a scout this is not optional: a FACT still needs a `file:line` you confirmed,
not one a sub-model reported.

**5. Return through `FINAL_VAR(name)`**, then `rlm.py final <session>` prints it in full.
That output — not the cell transcripts — is what goes into your `state.json` key.

## Cost and the budget

Every session has a dollar budget (default $1) and a call cap (default 200). Every call
reserves a worst-case estimate before it runs; calls wait for in-flight ones to settle
rather than overshoot, and the budget refuses a call outright only on money already spent.
A refused piece comes back from `llm_map` as `"[error] budget would be exceeded ..."` —
**count the errors before you combine**, or a refused chunk reads as "nothing found".

Measured on a 135KB file, 5 chunks: **$0.10, 79s wall**. `--effort none` (thinking off)
is ~2x cheaper and much faster, and gave a **wrong** answer on the same chunk — keep it
for literal extraction only.

Report `rlm.py cost <session>` in your key alongside the answer. Spend that nobody wrote
down is spend nobody can evaluate.

## Variables between cells

Anything that pickles persists across `exec` calls; `re` and `json` are preloaded.
Functions and lambdas do not persist — the cell footer names any that were dropped — so
define a helper in the cell that uses it. Paid sub-call results persist even when the
cell raises afterwards: assign `llm_map`'s result to a variable *first*, then process it.
