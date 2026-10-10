#!/usr/bin/env python
"""A Recursive Language Model REPL for fleet nodes. Stdlib only.

    python @FLEET@/.graph/rlm.py load  <session> <file>|-     # context = that text
    python @FLEET@/.graph/rlm.py exec  <session> <<'PY'       # run code against it
    print(len(context)); print(peek(0, 400))
    PY
    python @FLEET@/.graph/rlm.py final <session>              # print the FINAL_VAR answer
    python @FLEET@/.graph/rlm.py cost  <session>              # sub-calls, spend, budget

Run from the project root, like everything else in the fleet.

WHAT AN RLM IS
--------------
Zhang, Kraska & Khattab, "Recursive Language Models" (arXiv:2512.24601; blog:
alexzhang13.github.io/blog/2025/rlm/). The root model never reads the long input. It
sits in a REPL as the variable `context`; the root writes code to peek at it, grep it,
partition it, and call a cheaper sub-model over the pieces. Only a constant-size prefix
of each cell's output re-enters the root's history, so the root's window stays clean no
matter how large `context` is. The paper's headline configuration is depth 1 with a
cheaper sub-model (GPT-5 root, GPT-5-mini sub), and that is the shape here:

    root model      the node already running (scout, an orchestrator)
    REPL            this script, driven through Bash; state persists between `exec`s
    sub-model       headless `claude -p`, haiku by default, no tools, no settings

WHAT IS DIFFERENT FROM THE PAPER, deliberately
----------------------------------------------
  - `llm_map` runs sub-calls concurrently. The paper names blocking sub-calls as its
    main systems limitation; a thread pool over subprocesses removes it for free.
  - Spend is CAPPED, not just reported. The paper's cost tail comes from trajectories
    that flail; here a session has a dollar budget and a call cap, and every call is
    ledgered with its real `total_cost_usd`. The cap is enforced BEFORE a call, never
    during one: each call reserves a worst-case estimate (`estimate_usd`). A call that
    would pass the budget only because of calls still in flight WAITS for them to
    settle; one that would pass it on money already spent is refused. So concurrency
    cannot overshoot; only a call that costs more than its own estimate can, and the
    ledger records `over_estimate` when that happens.
  - Depth is 1, full stop. A sub-call has no tools, so it cannot open a REPL of its own.

WHY NOT `claude -p --max-budget-usd`, measured 2026-10-06
---------------------------------------------------------
It is checked AFTER the call. A 30KB haiku call capped at $0.003 spent $0.027, then
returned `error_max_budget_usd` with no result: paid in full, answer discarded. Passing
it would only turn an over-budget call into a wasted one. The first live run of this
script did pass it, with each concurrent call handed the whole remaining budget, and a
$0.10 session spent $0.237. That is why the reservation exists.

THINKING AND CACHING, measured 2026-10-06 on one 30KB chunk of a long state document
-------------------------------------------------------------------------------
    default effort, cached     $0.051   60s   6466 thinking tokens   answer correct
    thinking off               $0.019    2s      0                   answer WRONG (2 closed
                                                                     gaps listed as open)
    --effort low, no caching   $0.026   28s   2844                   answer correct
    --effort medium, no cache  $0.030   62s   5812                   same answer as low
So the default is `--effort low`, and prompt caching is off (`DISABLE_PROMPT_CACHING=1`):
a one-shot excerpt is never re-read, so a cache write is a 2x input surcharge for
nothing. `effort="none"` turns thinking off for pure extraction where the evidence is
literal (a grep could almost do it); the measurement says do not use it for judgment.

THE SUB-CALL IS ISOLATED FROM THE FLEET, and that is load-bearing
-----------------------------------------------------------------
`claude -p` launched from the project root would load the fleet's hooks and CLAUDE.md: every
sub-call would land in the open run's `activity.jsonl` as a phantom node, and pay for the
constitution as input. So each call runs with `--tools "" --setting-sources ""
--no-session-persistence`, a minimal `--system-prompt`, and a cwd OUTSIDE the project (the
session directory, under the system temp dir). Measured 2026-10-06: the default Claude
Code system prompt costs $0.0105 for a one-word haiku reply; the minimal one, $0.00076.

WHERE IT MUST NOT BE USED
-------------------------
  - In `.claude/hooks/**`. A sub-call is a network call, and no hook may depend on one.
  - By `reviewer`, `architect` or `integrator`. An RLM hands the reading to haiku, and
    those are the nodes GRAPH.md § Model tiering says are never downgraded. A reviewer
    reading a diff through haiku summaries is a laundered review.

STATE
-----
`$RLM_HOME/<session>/` (default: <tempdir>/graph-rlm/<session>/). Machine-local and
disposable -- nothing under the project root is written, so there is nothing to gitignore.

    context.txt   the loaded input, verbatim
    ns.pickle     the REPL namespace: every picklable variable you assigned
    calls.jsonl   one line per sub-call: model, effort, chars in/out, cost, estimate,
                  seconds, error, and over_estimate when the cost beat the estimate
    final.txt     the answer, written by FINAL_VAR(name)

Variables persist between `exec`s if they pickle. Functions and modules defined in a
cell do not -- redefine them, or keep them in one cell. The skipped names are listed.

TESTING
-------
`RLM_FAKE_LLM=1` swaps the sub-model for a deterministic offline stub that costs
$0.001 a call. The test suite runs entirely under it.
"""
import argparse
import contextlib
import io
import json
import os
import pickle
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import traceback
from concurrent.futures import ThreadPoolExecutor

DEFAULT_MODEL = "haiku"
DEFAULT_EFFORT = "low"
DEFAULT_BUDGET_USD = 1.00
DEFAULT_MAX_CALLS = 200
DEFAULT_OUTPUT_CHARS = 2000
DEFAULT_WORKERS = 4
SUB_TIMEOUT_S = 300

SUB_SYSTEM_PROMPT = (
    "You are a sub-model inside a recursive language model. You are given an excerpt "
    "of a larger document and a question about it. Answer only from the excerpt. Be "
    "terse and exact. If the excerpt does not contain the answer, say NONE."
)

# USD per MTok (in, out), GRAPH.md § Model tiering. An unknown model is priced as the
# dearest, so its estimate errs high and the budget refuses early rather than late.
PRICES = {"haiku": (1.0, 5.0), "sonnet": (3.0, 15.0), "opus": (5.0, 25.0)}
# Output reserved per call: covers --effort low/medium thinking on a 30KB excerpt
# (2844 and 5812 tokens measured) plus the answer.
EST_OUTPUT_TOKENS = 8000
EST_CHARS_PER_TOKEN = 3  # conservative; English prose runs ~4
FAKE_COST_USD = 0.001

RESERVED = {"context", "peek", "grep", "chunks", "llm_query", "llm_map", "FINAL_VAR",
            "budget", "re", "json"}


class RLMError(Exception):
    pass


class BudgetExceeded(RLMError):
    pass


def home():
    return os.environ.get("RLM_HOME") or os.path.join(tempfile.gettempdir(), "graph-rlm")


def session_dir(session):
    if not re.fullmatch(r"[A-Za-z0-9._-]+", session):
        raise RLMError("session name must be [A-Za-z0-9._-]+, got %r" % session)
    return os.path.join(home(), session)


# --------------------------------------------------------------------------- sub-call

def build_argv(model, effort):
    """The exact `claude -p` invocation. Tested: every isolation flag must be here, and
    `--max-budget-usd` must NOT be (see the docstring: it bills, then discards)."""
    exe = shutil.which("claude") or "claude"
    argv = [exe, "-p", "--model", model, "--tools", "", "--setting-sources", "",
            "--no-session-persistence", "--system-prompt", SUB_SYSTEM_PROMPT,
            "--output-format", "json"]
    if effort != "none":
        argv += ["--effort", effort]
    return argv


def build_env(effort):
    env = dict(os.environ, DISABLE_PROMPT_CACHING="1")
    if effort == "none":
        env["MAX_THINKING_TOKENS"] = "0"
    return env


def estimate_usd(chars_in, model, fake=False):
    """Worst-case cost of one call, reserved against the budget before it runs."""
    if fake:
        return FAKE_COST_USD
    p_in, p_out = PRICES.get(model, max(PRICES.values()))
    tokens_in = chars_in / EST_CHARS_PER_TOKEN + len(SUB_SYSTEM_PROMPT)
    return (tokens_in * p_in + EST_OUTPUT_TOKENS * p_out) / 1e6


def _fake_call(prompt):
    """Offline stub: deterministic, cheap, and lets a test see what was sent."""
    lines = prompt.strip().splitlines()
    return {"result": "FAKE[%d chars] %s" % (len(prompt), lines[-1][:60] if lines else ""),
            "total_cost_usd": FAKE_COST_USD, "is_error": False}


def _real_call(prompt, model, effort, cwd):
    proc = subprocess.run(build_argv(model, effort), input=prompt, cwd=cwd,
                          env=build_env(effort), capture_output=True, text=True,
                          encoding="utf-8", timeout=SUB_TIMEOUT_S)
    try:
        return json.loads(proc.stdout)
    except ValueError:
        raise RLMError("sub-call exit %d, unparseable output: %s"
                       % (proc.returncode, (proc.stderr or proc.stdout)[:300]))


class Ledger:
    """Spend and call accounting for one session, shared by every thread in a cell."""

    def __init__(self, sdir, budget_usd, max_calls):
        self.path = os.path.join(sdir, "calls.jsonl")
        self.sdir = sdir
        self.budget_usd = budget_usd
        self.max_calls = max_calls
        self.lock = threading.Condition()
        self.spent, self.calls, self.reserved = 0.0, 0, 0.0
        if os.path.exists(self.path):
            with open(self.path, encoding="utf-8") as f:
                for line in f:
                    if line.strip():
                        row = json.loads(line)
                        self.spent += row.get("cost_usd") or 0.0
                        self.calls += 1

    def reserve(self, estimate):
        """Claim a call slot and `estimate` dollars. In-flight reservations count against
        the budget, so N concurrent calls cannot each see it as free. A call that fits
        only once in-flight calls settle WAITS for them -- estimates are worst cases and
        usually return most of the reservation. It is refused only when what is already
        SPENT leaves no room."""
        with self.lock:
            while True:
                if self.calls >= self.max_calls:
                    raise BudgetExceeded("call cap reached: %d of %d"
                                         % (self.calls, self.max_calls))
                if self.spent + estimate > self.budget_usd:
                    raise BudgetExceeded(
                        "budget would be exceeded: $%.4f spent + $%.4f estimated > $%g"
                        % (self.spent, estimate, self.budget_usd))
                if self.spent + self.reserved + estimate <= self.budget_usd:
                    break
                self.lock.wait()
            self.calls += 1
            self.reserved += estimate

    def record(self, row, estimate):
        with self.lock:
            self.reserved -= estimate
            cost = row.get("cost_usd") or 0.0
            self.spent += cost
            if cost > estimate:
                row["over_estimate"] = round(cost - estimate, 6)
            with open(self.path, "a", encoding="utf-8") as f:
                f.write(json.dumps(row) + "\n")
            self.lock.notify_all()

    def summary(self):
        return {"calls": self.calls, "spent_usd": round(self.spent, 6),
                "budget_usd": self.budget_usd, "max_calls": self.max_calls}


# --------------------------------------------------------------------------- REPL helpers

def make_helpers(context, ledger, sdir, model, effort=DEFAULT_EFFORT):
    fake = bool(os.environ.get("RLM_FAKE_LLM"))

    def peek(start=0, length=2000):
        """context[start:start+length]. Negative start counts from the end."""
        if start < 0:
            start = max(len(context) + start, 0)
        return context[start:start + length]

    def grep(pattern, window=0, flags=re.IGNORECASE, limit=200):
        """[(line_no, text)] for lines matching `pattern`, with `window` lines either side
        joined into `text`. 1-based line numbers, so they can be quoted as file:line."""
        lines = context.splitlines()
        rx = re.compile(pattern, flags)
        out = []
        for i, line in enumerate(lines):
            if rx.search(line):
                lo, hi = max(i - window, 0), min(i + window + 1, len(lines))
                out.append((i + 1, "\n".join(lines[lo:hi])))
                if len(out) >= limit:
                    break
        return out

    def chunks(size=40000, overlap=0, by="lines"):
        """Partition context into pieces of about `size` chars. by="lines" never splits a
        line; by="chars" cuts exactly. Each piece is (start_line_or_offset, text)."""
        if by == "chars":
            step = max(size - overlap, 1)
            return [(i, context[i:i + size]) for i in range(0, len(context), step)]
        pieces, buf, n, start = [], [], 0, 1
        for i, line in enumerate(context.splitlines(keepends=True), 1):
            if buf and n + len(line) > size:
                pieces.append((start, "".join(buf)))
                buf, n, start = [], 0, i
            buf.append(line)
            n += len(line)
        if buf:
            pieces.append((start, "".join(buf)))
        return pieces

    def llm_query(prompt, piece=None, model=model, effort=effort):
        """One sub-model call. `piece` is the excerpt (a string or a chunks() tuple);
        `prompt` is the question. Returns the sub-model's text. Raises BudgetExceeded.
        effort: "low" (default), "medium", "high", or "none" for thinking off."""
        if isinstance(piece, tuple):
            piece = piece[1]
        full = prompt if piece is None else "<excerpt>\n%s\n</excerpt>\n\n%s" % (piece, prompt)
        estimate = estimate_usd(len(full), model, fake)
        ledger.reserve(estimate)
        t0, err, data = time.time(), None, {}
        try:
            data = _fake_call(full) if fake else _real_call(full, model, effort, sdir)
            if data.get("is_error"):
                err = str(data.get("result") or data.get("subtype") or "sub-call error")[:300]
        except Exception as e:  # recorded, then re-raised: a failed call still costs a slot
            err = "%s: %s" % (type(e).__name__, e)
        ledger.record({"ts": round(t0, 3), "model": "fake" if fake else model,
                       "effort": effort, "chars_in": len(full),
                       "chars_out": len(data.get("result") or ""),
                       "cost_usd": data.get("total_cost_usd"),
                       "estimate_usd": round(estimate, 6),
                       "seconds": round(time.time() - t0, 2), "error": err}, estimate)
        if err:
            raise RLMError(err)
        return data.get("result") or ""

    def llm_map(prompt, pieces, workers=DEFAULT_WORKERS, model=model, effort=effort):
        """llm_query(prompt, p) for every p, concurrently, results in input order. A piece
        that fails yields "[error] <reason>" instead of losing the batch's paid results."""
        def one(p):
            try:
                return llm_query(prompt, p, model=model, effort=effort)
            except RLMError as e:
                return "[error] %s" % e
        with ThreadPoolExecutor(max_workers=max(1, workers)) as pool:
            return list(pool.map(one, pieces))

    def budget():
        return ledger.summary()

    # FINAL_VAR is added by run_cell: it needs the cell's own namespace.
    return {"peek": peek, "grep": grep, "chunks": chunks, "llm_query": llm_query,
            "llm_map": llm_map, "budget": budget,
            "RLMError": RLMError, "BudgetExceeded": BudgetExceeded}


# --------------------------------------------------------------------------- commands

def _meta(sdir):
    with open(os.path.join(sdir, "meta.json"), encoding="utf-8") as f:
        return json.load(f)


def cmd_load(session, source, budget_usd, max_calls, model, effort=DEFAULT_EFFORT):
    sdir = session_dir(session)
    text = sys.stdin.read() if source == "-" else open(source, encoding="utf-8",
                                                       errors="replace").read()
    if os.path.exists(sdir):
        shutil.rmtree(sdir)
    os.makedirs(sdir)
    with open(os.path.join(sdir, "context.txt"), "w", encoding="utf-8", newline="") as f:
        f.write(text)
    with open(os.path.join(sdir, "meta.json"), "w", encoding="utf-8") as f:
        json.dump({"source": source, "budget_usd": budget_usd, "max_calls": max_calls,
                   "model": model, "effort": effort, "loaded": time.strftime("%Y-%m-%dT%H:%M:%S")}, f)
    print("rlm %s: context loaded -- %d chars, %d lines, from %s"
          % (session, len(text), text.count("\n") + 1, source))
    print("budget $%g, max %d sub-calls, sub-model %s at effort %s. Session dir: %s"
          % (budget_usd, max_calls, model, effort, sdir))
    return 0


def run_cell(session, code, output_chars=DEFAULT_OUTPUT_CHARS):
    """Execute one cell. Returns (exit_code, printed_text). Separated for the tests."""
    sdir = session_dir(session)
    if not os.path.exists(os.path.join(sdir, "context.txt")):
        return 2, "rlm %s: no context loaded -- run `rlm.py load %s <file>` first" % (session, session)
    meta = _meta(sdir)
    with open(os.path.join(sdir, "context.txt"), encoding="utf-8", newline="") as f:
        context = f.read()
    ledger = Ledger(sdir, meta["budget_usd"], meta["max_calls"])
    ns_path = os.path.join(sdir, "ns.pickle")
    ns = {}
    if os.path.exists(ns_path):
        with open(ns_path, "rb") as f:
            ns = pickle.load(f)
    ns.update(make_helpers(context, ledger, sdir, meta["model"],
                           meta.get("effort", DEFAULT_EFFORT)))
    ns["context"] = context
    ns["re"], ns["json"] = re, json
    final = {}

    def FINAL_VAR(name):
        if name not in ns:
            raise RLMError("FINAL_VAR: no variable named %r" % name)
        value = ns[name]
        text = value if isinstance(value, str) else json.dumps(value, indent=2, default=str)
        with open(os.path.join(sdir, "final.txt"), "w", encoding="utf-8") as f:
            f.write(text)
        final["chars"] = len(text)
    ns["FINAL_VAR"] = FINAL_VAR

    calls_before = ledger.calls
    buf = io.StringIO()
    code_rc = 0
    with contextlib.redirect_stdout(buf), contextlib.redirect_stderr(buf):
        try:
            exec(compile(code, "<rlm-cell>", "exec"), ns)
        except Exception:
            traceback.print_exc(limit=3)
            code_rc = 1

    kept, skipped = {}, []
    for k, v in ns.items():
        if k.startswith("__") or k in RESERVED or k in ("RLMError", "BudgetExceeded"):
            continue
        try:
            pickle.dumps(v)
            kept[k] = v
        except Exception:
            skipped.append(k)
    with open(ns_path, "wb") as f:
        pickle.dump(kept, f)

    out = buf.getvalue()
    lines = []
    if len(out) > output_chars:
        lines.append(out[:output_chars])
        lines.append("[... output truncated: %d of %d chars shown. Assign it to a variable "
                     "and peek, rather than printing it whole.]" % (output_chars, len(out)))
    elif out:
        lines.append(out.rstrip("\n"))
    s = ledger.summary()
    tail = "[rlm] sub-calls this cell: %d, session: %d calls, $%.4f of $%.2f" % (
        ledger.calls - calls_before, s["calls"], s["spent_usd"], s["budget_usd"])
    if skipped:
        tail += ", not persisted (unpicklable): %s" % ", ".join(sorted(skipped))
    if final:
        tail += ", FINAL_VAR written, %d chars -- `rlm.py final %s`" % (final["chars"], session)
    lines.append(tail)
    return code_rc, "\n".join(lines)


def cmd_exec(session, output_chars):
    rc, text = run_cell(session, sys.stdin.read(), output_chars)
    print(text)
    return rc


def cmd_final(session):
    path = os.path.join(session_dir(session), "final.txt")
    if not os.path.exists(path):
        print("rlm %s: no answer yet -- call FINAL_VAR(name) in a cell" % session)
        return 1
    with open(path, encoding="utf-8") as f:
        sys.stdout.write(f.read())
    return 0


def cmd_cost(session):
    sdir = session_dir(session)
    if not os.path.exists(os.path.join(sdir, "meta.json")):
        print("rlm %s: no such session" % session)
        return 2
    meta = _meta(sdir)
    ledger = Ledger(sdir, meta["budget_usd"], meta["max_calls"])
    rows = []
    if os.path.exists(ledger.path):
        with open(ledger.path, encoding="utf-8") as f:
            rows = [json.loads(l) for l in f if l.strip()]
    errors = sum(1 for r in rows if r.get("error"))
    secs = sum(r.get("seconds") or 0 for r in rows)
    print("rlm %s: %d sub-calls, %d errors, $%.4f of $%.2f, %.1fs summed sub-call time"
          % (session, len(rows), errors, ledger.spent, meta["budget_usd"], secs))
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(prog="rlm.py", description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("load")
    p.add_argument("session")
    p.add_argument("source", help="a file path, or - for stdin")
    p.add_argument("--budget", type=float, default=DEFAULT_BUDGET_USD, help="USD cap")
    p.add_argument("--max-calls", type=int, default=DEFAULT_MAX_CALLS)
    p.add_argument("--model", default=DEFAULT_MODEL, help="sub-model (default haiku)")
    p.add_argument("--effort", default=DEFAULT_EFFORT,
                   choices=["none", "low", "medium", "high"], help="sub-model thinking")
    p = sub.add_parser("exec")
    p.add_argument("session")
    p.add_argument("--output-chars", type=int, default=DEFAULT_OUTPUT_CHARS)
    for name in ("final", "cost"):
        sub.add_parser(name).add_argument("session")
    a = ap.parse_args(argv)
    try:
        if a.cmd == "load":
            return cmd_load(a.session, a.source, a.budget, a.max_calls, a.model, a.effort)
        if a.cmd == "exec":
            return cmd_exec(a.session, a.output_chars)
        if a.cmd == "final":
            return cmd_final(a.session)
        return cmd_cost(a.session)
    except RLMError as e:
        print("rlm: %s" % e, file=sys.stderr)
        return 2


if __name__ == "__main__":
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    sys.exit(main())
