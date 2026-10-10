#!/usr/bin/env python
"""Heartbeat: append one line per node event to the open run's activity log.

The overseer this fleet wanted is not an agent. A subagent is spawned, runs, returns
text and ends -- there is no loop it could watch from, no channel to its siblings, and a
node that consumes nothing and produces nothing is the fake edge `GRAPH.md` says to
delete. The hook layer, by contrast, already runs alongside every node and is handed
`agent_id` and `agent_type` on every tool event, plus `SubagentStart`/`SubagentStop`.

So this writes `.graph/runs/<run>/activity.jsonl`: one compact JSON object per line.

    {"t": 1756209600.4, "ev": "start", "agent": "builder",  "id": "abc123"}
    {"t": 1756209601.9, "ev": "tool",  "agent": "builder",  "id": "abc123", "tool": "Edit", "tokens": 4213, "say": "Adding the retry wrapper around fetchState."}
    {"t": 1756209640.2, "ev": "stop",  "agent": "builder",  "id": "abc123", "tokens": 5890, "say": "Done -- all three call sites now retry."}

Which is enough for four things the fleet could not previously answer:

  live status      what is running right now, and what it just did
  model tiering    GRAPH.md argues cost hard and has never measured it: duration and
                   tool counts per agent_type are the evidence
  independence     a re-review's `SubagentStart` carries a NEW agent_id, which is what
                   makes "a fresh reviewer" checkable rather than merely asserted
  real parallelism when a diamond finally runs, overlapping timestamps are what prove
                   the builders actually ran concurrently

`agent_type` is absent for the main session, so those events are recorded as
`orchestrator` -- it is a participant in the run and its writes belong in the record.

`parent` (`parent_tool_use_id`, when the payload carries one) is recorded too, added
2026-09-09 to diagnose gap #20: 1,411 of 1,496 `stop` events carry an `agent_id` that
never had a matching `start`, are stamped `orchestrator` for lack of `agent_type`, and
interleave with live work rather than clustering at a run's end -- not what "a subagent
finished" should look like. The leading hypothesis is that the harness fires
`SubagentStop` for something that isn't a `Task`-tool spawn -- a backgrounded `Bash`
process, or an internal tool-use agent behind `WebSearch`/`WebFetch` -- and `parent`
is the field that would show a phantom nested under a real node's own `tool_use_id`
rather than orphaned. Unverified; this only logs the field, it draws no conclusion.

`tokens` (added 2026-09-11) is a real, per-agent-id token total -- NOT derived from the
hook payload, which carries no usage field at all (checked against the hooks reference).
Every subagent writes its own transcript to a location Claude Code owns, not this fleet:
`~/.claude/projects/<encoded cwd>/<session_id>/subagents/agent-<agent_id>.jsonl`, one line
per turn, each carrying a real `message.usage` block (`input_tokens`, `output_tokens`,
`cache_creation_input_tokens`, `cache_read_input_tokens`). On every `tool` and `stop` event
that carries an `agent_id`, this sums those four fields across every line of that specific
agent's transcript and writes the running total as `tokens` -- a cumulative count for that
one instance, so the last value written is that instance's true final count.

`say` (added 2026-09-11) is the same idea applied to what an agent is actually saying, not
just how much it costs. The same transcript line that carries `message.usage` also carries
`message.content`, a list of blocks -- `text` blocks are the agent's own prose, `tool_use`
blocks are calls. `last_said_by` walks the transcript and keeps the newest `text` block it
finds, collapsed to one line and capped at 220 chars so activity.jsonl stays a heartbeat
log and not a second copy of the transcript. Written as `say` on the same `tool`/`stop`
events as `tokens`, from the same file read -- a snapshot of the latest thing said, not a
running total, so unlike `tokens` a later write simply REPLACES the field rather than
accumulating it.

Both are a real coupling to an undocumented, internal Claude Code storage layout rather
than to any documented hook field -- accepted deliberately (see the board's token-counter
work) because the alternative is fabricating a number or a quote, and unwritten if the
layout doesn't match: `tokens_used_by` returns `None` on any read/parse failure and
`last_said_by` returns `None` when no text block is found, and each field is simply
omitted from the line, exactly like every other best-effort field here.

Silent when no run is open or the run is closed. Never blocks, never raises: an
unwritable log must not cost a tool call. Exit 0 always.
"""
import json
import os
import re
import sys
import time

TOKEN_FIELDS = ("input_tokens", "output_tokens",
                 "cache_creation_input_tokens", "cache_read_input_tokens")


def encode_cwd(cwd):
    """Mirror Claude Code's own project-directory name for a launch cwd.

    Observed, not documented: Claude Code names a project's directory after its launch
    cwd -- every `\\`, `/` and `:` becomes `-`,
    everything else is left alone.
    """
    return re.sub(r"[\\/:]", "-", cwd)


def subagent_transcript_path(payload):
    """Where Claude Code writes ONE subagent's own transcript, or None.

    Needs `cwd`, `session_id` and `agent_id` -- all common hook fields -- present
    together. Absence of any one means this can't be located; that is not this
    fleet's business to fix, just to skip.
    """
    cwd = payload.get("cwd")
    session_id = payload.get("session_id")
    agent_id = payload.get("agent_id")
    if not (cwd and session_id and agent_id):
        return None
    home = os.path.expanduser("~")
    return os.path.join(home, ".claude", "projects", encode_cwd(str(cwd)),
                         str(session_id), "subagents", "agent-" + str(agent_id) + ".jsonl")


def tokens_used_by(path):
    """Sum of every token field across every turn in one subagent's transcript.

    A running total, not a delta -- callers write it as-is and the last line
    written for a given agent_id is that agent's true final count. Returns None
    (never 0) when the file can't be read at all, so a missing transcript stays
    silent instead of rendering a fabricated zero.
    """
    try:
        fh = open(path, encoding="utf-8")
    except OSError:
        return None
    total = 0
    with fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                obj = json.loads(line)
            except ValueError:
                continue        # a torn final line while Claude Code is mid-append
            usage = ((obj.get("message") or {}).get("usage")
                     if isinstance(obj, dict) else None)
            if not isinstance(usage, dict):
                continue
            for field in TOKEN_FIELDS:
                value = usage.get(field)
                if isinstance(value, (int, float)):
                    total += value
    return total


SAY_MAX_CHARS = 220


def last_said_by(path):
    """The newest thing an agent's own transcript has it saying, or None.

    Walks every turn in the transcript looking for `message.content` blocks of
    type "text" (an agent's own prose, as opposed to a "tool_use" block); keeps
    only the latest one found, so a later turn always overwrites an earlier one.
    Collapsed to one line and capped so this stays a caption, not a transcript
    excerpt. Returns None when the file can't be read, is empty, or never once
    contains a text block -- never a fabricated placeholder string.
    """
    try:
        fh = open(path, encoding="utf-8")
    except OSError:
        return None
    said = None
    with fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                obj = json.loads(line)
            except ValueError:
                continue        # a torn final line while Claude Code is mid-append
            content = ((obj.get("message") or {}).get("content")
                       if isinstance(obj, dict) else None)
            if not isinstance(content, list):
                continue
            for block in content:
                if isinstance(block, dict) and block.get("type") == "text":
                    text = block.get("text")
                    if isinstance(text, str) and text.strip():
                        said = text.strip()
    if said is None:
        return None
    said = " ".join(said.split())
    if len(said) > SAY_MAX_CHARS:
        said = said[:SAY_MAX_CHARS - 1].rstrip() + "…"
    return said

# ~1s of total patience: 50 tries, 20ms apart. A node event is worth waiting a moment
# for and worth nothing at all if the hook blocks the tool call that produced it.
LOCK_TRIES = 50
LOCK_WAIT = 0.02


def _lock(fd):
    """Take an exclusive advisory lock on a sidecar fd. True if held, False if not."""
    try:
        import fcntl
        fcntl.flock(fd, fcntl.LOCK_EX)
        return True
    except ImportError:
        pass
    try:
        import msvcrt
        for _ in range(LOCK_TRIES):
            try:
                msvcrt.locking(fd, msvcrt.LK_NBLCK, 1)
                return True
            except OSError:
                time.sleep(LOCK_WAIT)
    except Exception:
        return False
    return False


def _unlock(fd):
    try:
        import fcntl
        fcntl.flock(fd, fcntl.LOCK_UN)
        return
    except ImportError:
        pass
    try:
        import msvcrt
        os.lseek(fd, 0, os.SEEK_SET)
        msvcrt.locking(fd, msvcrt.LK_UNLCK, 1)
    except Exception:
        pass


def append_line(path, text):
    """Append ONE line, serialised against every other process writing this log.

    Every node in a run appends to the same `activity.jsonl`, concurrently and from
    separate processes -- a diamond has three builders and three reviewers live at once,
    each firing this hook on every tool call. Buffered text-mode `open(path, "a")` does
    NOT make that safe: the handle carries its own file position and its own buffer, so
    two interleaved appends can land on top of each other.

    That is not hypothetical here. one run's log line 1225 reads
    `"Bash"}` -- the tail of an event whose head was overwritten by another process's
    append. The event it recorded is gone, not merely mangled. Readers (`brief.py`,
    and others) skip unparseable lines, so it cost no crash and left no report; it
    silently subtracted one event from the evidence base they are built on.

    **`O_APPEND` alone does not fix this on Windows, and believing it did was the first
    attempt.** On POSIX the seek-to-end and the write are one atomic operation; the
    Windows CRT emulates `_O_APPEND` as a seek followed by a write, and the gap between
    them is the whole bug. Measured on this machine, 2026-09-19, four processes appending
    150 lines each to one file: **471 of 600 events survived** -- and with zero torn
    lines, so the loss is invisible to any check that only asks whether each line parses.
    Under the old buffered `open(path, "a")` the same run produced the torn-tail shape
    seen in the real log. Two different symptoms, one cause.

    So the write is serialised by an advisory lock on a sidecar `.lock` file: `flock`
    where there is one, `msvcrt.locking` where there is not. The lock is on a sidecar
    rather than on the log itself because a Windows lock is mandatory, not advisory --
    locking the log would make `brief.py` and other readers fail to READ it mid-run,
    turning a write-safety fix into a read outage.

    Failure to take the lock is not a reason to drop the event: after ~1s of contention
    it writes anyway. A log line that might collide is worth more than a node event that
    certainly never existed, and this hook must never be the reason a run stalls.
    """
    flags = os.O_WRONLY | os.O_CREAT | os.O_APPEND | getattr(os, "O_BINARY", 0)
    payload = (text + "\n").encode("utf-8")
    try:
        lock_fd = os.open(path + ".lock", os.O_RDWR | os.O_CREAT, 0o644)
    except OSError:
        lock_fd = None

    held = _lock(lock_fd) if lock_fd is not None else False
    try:
        fd = os.open(path, flags, 0o644)
        try:
            os.lseek(fd, 0, os.SEEK_END)   # explicit: do not trust O_APPEND to have done it
            os.write(fd, payload)
        finally:
            os.close(fd)
    finally:
        if lock_fd is not None:
            if held:
                _unlock(lock_fd)
            os.close(lock_fd)


HERE = os.path.dirname(os.path.realpath(__file__))
FLEET = os.path.normpath(os.path.join(HERE, "..", ".."))
CURRENT = os.path.join(FLEET, ".graph", "CURRENT")
# Aligned with `guard-builder-scope.py` and `show-board.py`, which have always carried
# all three. `parked` was missing here until 2026-09-19, so a pointer left on a parked
# run -- one run has sat parked since the day it opened -- kept
# collecting events from unrelated later sessions into a run that had stopped, while the
# board stayed correctly silent about it.
CLOSED = ("done", "blocked", "parked")
MAX_LINES = 20000        # a runaway loop must not fill a disk


def open_run_dir():
    try:
        with open(CURRENT, encoding="utf-8") as fh:
            run_id = fh.read().strip()
        if not run_id:
            return None
        run_dir = os.path.join(FLEET, ".graph", "runs", run_id)
        with open(os.path.join(run_dir, "state.json"), encoding="utf-8") as fh:
            state = json.load(fh)
    except (OSError, ValueError):
        return None
    if not isinstance(state, dict):
        return None
    if str(state.get("status") or "").strip().lower() in CLOSED:
        return None
    return run_dir


def main():
    try:
        payload = json.load(sys.stdin)
    except Exception:
        return

    event = {"SubagentStart": "start", "SubagentStop": "stop",
             "PostToolUse": "tool"}.get(payload.get("hook_event_name"))
    if event is None:
        return

    run_dir = open_run_dir()
    if run_dir is None:
        return

    line = {
        "t": round(time.time(), 1),
        "ev": event,
        "agent": str(payload.get("agent_type") or "orchestrator").strip(),
    }
    if payload.get("agent_id"):
        line["id"] = str(payload["agent_id"])
    if payload.get("parent_tool_use_id"):
        line["parent"] = str(payload["parent_tool_use_id"])
    if event == "tool" and payload.get("tool_name"):
        line["tool"] = str(payload["tool_name"])
    if event in ("tool", "stop") and payload.get("agent_id"):
        tpath = subagent_transcript_path(payload)
        tokens = tokens_used_by(tpath) if tpath else None
        if tokens is not None:
            line["tokens"] = tokens
        say = last_said_by(tpath) if tpath else None
        if say is not None:
            line["say"] = say

    path = os.path.join(run_dir, "activity.jsonl")
    try:
        # Cheap guard, checked only occasionally: counting every line on every tool
        # call would make the log cost more than the work it records.
        if event == "start" and os.path.isfile(path):
            with open(path, encoding="utf-8") as fh:
                if sum(1 for _ in fh) >= MAX_LINES:
                    return
        append_line(path, json.dumps(line, separators=(",", ":")))
    except OSError:
        return


main()
