#!/usr/bin/env python
"""Machine check for GRAPH.md section 3 rule 3's three-line return cap.

    python @FLEET@/.graph/check-return-cap.py [<run-id>]

Read-only report. GRAPH.md section 3 rule 3: "Return a HEADLINE, not the handoff.
Three lines, no verbatim command output, no file lists, no findings bodies... `architect`
and `ops` are the two exceptions: their returns are **gate** material, not status." Prose
since 2026-09-03 (gap #19), with nothing on disk that ever checked it. This is that check.

**Why transcripts, not state.json.** `state.json` carries no return text at all -- rule 2
puts a node's WORK in its own key, and rule 3's return is deliberately something
different: text printed once to the orchestrator's own tab, never written anywhere by the
node itself. `record-activity.py`'s `say` field looks tempting but is not it -- it is a
ONE-LINE, 220-char-capped caption (`record-activity.py` SAY_MAX_CHARS, line 139), built to
be a heartbeat caption, not a verbatim return, and structurally incapable of counting
lines in a return that might itself run to three lines of up to 220 chars each. The only
place the real return text survives is the transcript Claude Code itself writes for that
one agent instance:

    ~/.claude/projects/<encoded cwd>/<session_id>/subagents/agent-<agent_id>.jsonl

That is the same undocumented, internal storage layout `record-activity.py`'s `tokens`/
`say` fields already lean on (see that file's module docstring) -- taken again here,
deliberately, for the same reason: the alternative is not checking the rule at all.
The path derivation below is COPIED
from `record-activity.py`, not imported -- a checker importing a hook would inherit the
hook's own failure modes (silent-on-stdin-error, exit 0 always) as its own, which are the
wrong defaults for a script whose whole job is to report a violation with a real exit code.

**What "the return" actually is (attempt-1 defect, fixed here).** A node's return is not
"whatever text it said last" -- it is specifically the `message` argument of its own
`SubagentHandback` tool_use call, the one mechanism that actually delivers text to the
orchestrator's tab (see the node files' own Return sections: "call `SubagentHandback`
... and then stop"). The transcript is a full agent turn log, and a `text` block can
appear before that call (working narration on the way to the handback) or, harness-
permitting, after it (post-handback chatter the orchestrator never received and rule 3
never governed). Attempt 1 took the newest `text` block anywhere in the transcript and
was wrong on live data because of exactly that: it reported 4 and 10 lines for two nodes
whose real, delivered returns were 3 and 2. `last_return_text` below reads the
`SubagentHandback` tool_use's own `message` input and nothing else.

**A found-and-over-cap return is the only failure this reports.** Every other outcome --
no activity.jsonl, an agent_id with no transcript on disk, a transcript with no
`SubagentHandback` call at all (an in-flight lane that has not returned yet, same as
attempt 1's second defect: it must never be certified compliant off an early `text`
block), a torn line mid-transcript -- is `unverifiable`, counted and printed, never a
reason to fail: absence of evidence is not evidence of a violation, the same
silent-on-failure rule `last_said_by` follows. Exit 1 is reserved for a return actually
found on disk and over cap.

**A compliant `state.json` key does not excuse an over-cap return.** The cap protects the
orchestrator's context window, spent the moment the return is printed to its own tab -- a
tidy `builders.<slice>` key written a moment later does not un-spend it. `state.json` is
read here only to confirm the run exists; it is never consulted for return content.

**Never echoes transcript text.** Not one character of the checked prose reaches this
script's own output -- agent type, agent id and line count only. A checker that quotes
what it polices turns every report into the transcript excerpt rule 3 exists to prevent.

Read-only, pure stdlib. Exit 0 unless a real over-cap return is found on disk.
"""
import glob
import importlib.util
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
FLEET = os.path.dirname(HERE)
CURRENT = os.path.join(HERE, "CURRENT")
VERIFY = os.path.join(HERE, "verify-state.py")

# GRAPH.md section 3 rule 3: "architect and ops are the two exceptions: their returns
# are gate material, not status, and a human cannot approve a summary of a plan or a
# deploy they have not been shown." Data, not prose -- a new exception is a diff to this
# set, not a sentence someone has to remember to write elsewhere.
EXEMPT = {"architect", "ops"}

CAP = 3


def load_module(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


# --------------------------------------------------- transcript location (copied from
# .claude/hooks/record-activity.py -- copied, not imported)

def encode_cwd(cwd):
    """Mirror Claude Code's own project-directory name for a launch cwd.

    Observed, not documented: Claude Code names a project's directory after its launch
    cwd -- every `\\`, `/` and `:` becomes `-`,
    everything else is left alone.
    """
    return re.sub(r"[\\/:]", "-", cwd)


def _mtime(path):
    try:
        return os.path.getmtime(path)
    except OSError:
        return 0.0


def find_transcript(agent_id, projects_root):
    """The one subagent transcript for `agent_id` under `projects_root`, or None.

    Unlike `record-activity.py`'s `subagent_transcript_path`, this script is never
    handed a hook payload carrying `session_id` -- `activity.jsonl` never stores one (no
    event line does; checked). So instead of building the one path a payload would name,
    this SEARCHES every session directory under the launch cwd's project root for the one
    file named `agent-<agent_id>.jsonl` under a `subagents/` sibling. Zero matches, or an
    unreadable root, is None, never an exception. More than one match (an agent_id
    reused across sessions, which should not happen with Claude Code's own ids) takes the
    most recently modified -- the least surprising tiebreak, not an adjudication.
    """
    if not agent_id:
        return None
    pattern = os.path.join(projects_root, "*", "subagents",
                            "agent-%s.jsonl" % agent_id)
    try:
        matches = glob.glob(pattern)
    except OSError:
        return None
    if not matches:
        return None
    matches.sort(key=_mtime, reverse=True)
    return matches[0]


def last_return_text(path):
    """The `message` argument of the newest `SubagentHandback` tool_use call in one
    agent's own transcript, or None.

    This is deliberately NOT "the newest `text` block" -- a `text` block can precede the
    handback (narration on the way to the call) or, harness-permitting, follow it (the
    agent kept talking after the tool call that already delivered its return; observed
    live, 2026-09-20, on this run's own scout transcript). Only a `SubagentHandback`
    call's own `message` field is the text rule 3 actually caps -- the node files agree:
    "call `SubagentHandback` ... and then stop." No such call anywhere in the transcript
    means the node has not returned (yet, or ever) -- None, correctly read by `check()`
    below as unverifiable, never a fabricated placeholder and never a compliant-by-
    default short answer lifted from an in-flight lane's opening sentence.

    Uncapped and uncollapsed, unlike `record-activity.py`'s `last_said_by`: that function
    exists to build a 220-char heartbeat caption; this one exists to count the real
    return's own lines, and truncating or collapsing the text first would make the count
    meaningless. If more than one `SubagentHandback` call appears (should not happen --
    every node file says "and then stop" -- but nothing stops it in the harness) the
    LATEST one wins, same "newest wins" rule as everywhere else in this file. A torn
    final line (the transcript mid-append) is skipped, not raised.
    """
    try:
        fh = open(path, encoding="utf-8")
    except OSError:
        return None
    returned = None
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
                if not isinstance(block, dict):
                    continue
                if block.get("type") != "tool_use" or block.get("name") != "SubagentHandback":
                    continue
                message = (block.get("input") or {}).get("message")
                if isinstance(message, str) and message.strip():
                    returned = message
    return returned


def line_count(text):
    """Non-empty lines of `text` after `strip()` -- a 3-line block with a trailing
    newline (or trailing blank lines) is 3, not 4: blank and whitespace-only lines are
    not counted (GRAPH.md section 3 rule 3's cap protects content, not formatting)."""
    return len([ln for ln in text.strip().splitlines() if ln.strip()])


# ------------------------------------------------------------------- lane identity
# (a lone `stop` event with
# a fresh id, never seen on a `start` or `tool` event, is a phantom lane and must not be
# handed a return to check at all.)

def agent_types(run_dir):
    """agent_id -> agent_type, for every id with a real `start` or `tool` event in this
    run's activity.jsonl. Missing, empty, or unparseable (any torn line) activity.jsonl
    is simply no lanes -- never a traceback."""
    path = os.path.join(run_dir, "activity.jsonl")
    try:
        with open(path, encoding="utf-8") as fh:
            events = [json.loads(line) for line in fh if line.strip()]
    except (OSError, ValueError):
        return {}
    types = {}
    for event in events:
        if not isinstance(event, dict):
            continue
        if event.get("ev") not in ("start", "tool"):
            continue
        agent_id = event.get("id")
        if not agent_id:
            continue
        types[str(agent_id)] = str(event.get("agent") or "?")
    return types


# ---------------------------------------------------------------------------- report

def check(run_dir, launch_cwd):
    """(violations, unverifiable, checked) -- three lists of report lines, agent_id order."""
    types = agent_types(run_dir)
    projects_root = os.path.join(os.path.expanduser("~"), ".claude", "projects",
                                  encode_cwd(launch_cwd))
    violations, unverifiable, checked = [], [], []
    for agent_id in sorted(types):
        agent_type = types[agent_id]
        transcript = find_transcript(agent_id, projects_root)
        text = last_return_text(transcript) if transcript else None
        if text is None:
            unverifiable.append("  unverifiable: %-12s %s -- no return recoverable"
                                 % (agent_type, agent_id))
            continue
        n = line_count(text)
        exempt = agent_type in EXEMPT
        checked.append("  %-12s %s -- %d line(s)%s"
                        % (agent_type, agent_id, n, " (exempt)" if exempt else ""))
        if not exempt and n > CAP:
            violations.append("  OVER CAP: %-12s %s -- %d line(s), cap is %d"
                               % (agent_type, agent_id, n, CAP))
    return violations, unverifiable, checked


def main(argv):
    run_id = argv[0] if argv else None
    if not run_id:
        try:
            with open(CURRENT, encoding="utf-8") as fh:
                run_id = fh.read().strip()
        except OSError:
            run_id = None
    if not run_id:
        sys.stderr.write("check-return-cap: no run given, and .graph/CURRENT names none\n")
        return 0        # nothing to check is not a violation

    verify = load_module(VERIFY, "verify_state")
    try:
        state, path = verify.load(run_id)
    except SystemExit:
        return 0         # unreadable run: unverifiable, never a violation
    if not isinstance(state, dict):
        return 0

    run_dir = os.path.dirname(path)
    violations, unverifiable, checked = check(run_dir, os.getcwd())

    print("check-return-cap: %s (cap=%d, exempt=%s)"
          % (run_id, CAP, ", ".join(sorted(EXEMPT))))
    if not checked and not unverifiable:
        print("  no node instances recorded -- nothing to check")
        return 0
    for line in checked:
        print(line)
    if unverifiable:
        print("  %d node(s) unverifiable" % len(unverifiable))
        for line in unverifiable:
            print(line)
    if violations:
        print("  %d violation(s):" % len(violations))
        for line in violations:
            print(line)
        return 1
    print("  all checked returns are within cap")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
