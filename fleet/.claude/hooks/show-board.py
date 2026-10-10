#!/usr/bin/env python
"""PostToolUse hook: print the run board to the human when a node is dispatched.

Fires on the `Agent` tool, the orchestrator's spawn call. It renders `.graph/brief.py` and
returns it as `systemMessage`, the documented way to put text in front of the user:

    {"systemMessage": "<the board>"}

**Why not `SubagentStop`, which is the obvious event.** Checked against the hooks
reference on 2026-09-03, and it is closed on both routes:

    "On display events like `Stop` and `SubagentStop`, stdout is added to Claude's
     context as a system message instead of being shown in the transcript, even if it
     doesn't parse as JSON."

and the field that works everywhere else is exempted on the same events -- `systemMessage`
"on display events like `Stop` and `SubagentStop` [...] is added to Claude's context
instead of being shown in the transcript." So a board emitted there reaches the
orchestrator, which already knows, and never reaches the person reading the main tab.

The fleet's own heartbeat says `SubagentStop` is the wrong trigger anyway. Across the four
runs carrying `activity.jsonl`, 471 of 503 `stop` events arrive with no `agent_type`, each
with a unique `agent_id` that never had a matching `SubagentStart`, interleaved with a
still-running node's tool calls. It is not a "a subagent finished" signal here.

**WHEN this fires, measured rather than assumed (2026-09-03).** This hook was written
believing `Agent` completes when the subagent it spawned finishes. It does not. Across the
five runs carrying a heartbeat, all 32 recorded `Agent` events land 0.0-0.2s after a
`SubagentStart`, and the nearest real subagent stop is between 3.5s and 1772s away -- never
once the other way round. Subagents run in the background and the spawn call returns a
handle immediately, so this is a **dispatch** board: it shows the run as the node picks it
up, before that node has written anything.

That is still worth printing -- it is the "off we go" card, and it costs nobody a token --
but it means **no hook prints a board when a node comes back**. `SubagentStop` would, and
cannot reach the human. So the post-return board is the orchestrator's own, by hand
(`feature-graph` § the board). Two boards per node, from two different actors, on purpose.

`systemMessage` also enters the orchestrator's context. That cost is real and accepted --
about ten lines per dispatch, derived from a file the orchestrator already owns.

ASCII glyphs always. `brief.py` picks glyphs from `sys.stdout.encoding`, which for a hook
is a pipe and tells us nothing about the terminal that will render this; a status line is
not worth a mojibake risk.

Silent when there is no open run, when the run is closed, when the tool is not `Agent`,
and when the caller is a subagent rather than the main session. Never blocks, never
raises: exit 0 always. A board that fails to render must cost nothing.
"""
import importlib.util
import json
import os
import sys

HERE = os.path.dirname(os.path.realpath(__file__))
FLEET = os.path.normpath(os.path.join(HERE, "..", ".."))
CURRENT = os.path.join(FLEET, ".graph", "CURRENT")
BRIEF = os.path.join(FLEET, ".graph", "brief.py")
CLOSED = ("done", "blocked", "parked")


def board_for(run_dir):
    """The rendered board, or None. `brief.py` owns every rule about what it says."""
    spec = importlib.util.spec_from_file_location("brief", BRIEF)
    brief = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(brief)
    path = os.path.join(run_dir, "state.json")
    with open(path, encoding="utf-8") as fh:
        state = json.load(fh)
    if not isinstance(state, dict):
        return None
    if str(state.get("status") or "").strip().lower() in CLOSED:
        return None
    return "\n".join(brief.board(state, path, brief.ASCII))


def open_run_dir():
    """The directory of the run `.graph/CURRENT` names, or None."""
    try:
        with open(CURRENT, encoding="utf-8") as fh:
            run_id = fh.read().strip()
    except OSError:
        return None
    if not run_id:
        return None
    run_dir = os.path.join(FLEET, ".graph", "runs", run_id)
    return run_dir if os.path.isfile(os.path.join(run_dir, "state.json")) else None


def main():
    try:
        payload = json.load(sys.stdin)
    except Exception:
        return
    if not isinstance(payload, dict):
        return
    if payload.get("hook_event_name") != "PostToolUse":
        return
    if payload.get("tool_name") != "Agent":
        return
    # The main session only. No node is granted the `Agent` tool today, but a nested
    # spawn must not print a board into a subagent's own tab.
    if str(payload.get("agent_type") or "").strip():
        return

    run_dir = open_run_dir()
    if run_dir is None:
        return
    try:
        board = board_for(run_dir)
    except Exception:
        return
    if not board:
        return
    sys.stdout.write(json.dumps({"systemMessage": board}))


main()
