#!/usr/bin/env python
"""PreToolUse hook: a builder may not write outside the file set the human approved.

The gate in `feature-graph` step 4 shows a human a plan whose slices name specific
files, and the human approves *that*. Until now nothing checked it afterwards -- a
builder could edit anything in the target and the approval meant only that the run was
allowed to start. This makes the approved file set a boundary instead of a description.

DENIES rather than warns. A warning after the write has already happened is a record of
the violation, not a guard against it, and `PreToolUse` is the only place in this fleet
where a rule can actually stop something.

Scope, stated honestly:

  It compares against the UNION of every slice's `files`, not the acting builder's own
  slice. A hook is given `agent_id`, never a slice id, and nothing maps one to the other.
  In `single-loop` -- which is every run this fleet has executed -- the union IS the one
  slice, so the guard is exact. In a diamond, worktrees already keep builders out of each
  other's trees, so what the union still buys is the plan boundary itself.

  It matches a builder working in a linked git WORKTREE by repo-relative path, since a
  worktree's absolute paths are rooted outside the project and can never equal the
  plan's. The repo is the project root (UMBRELLA = dirname(FLEET) assumes the fleet
  directory sits directly under the project root) and a plan entry is a path inside it.
  If that ever stops being true the
  entry fails to match and only the absolute rule applies -- the guard narrows, never
  widens.

  It fires only for `agent_type == "builder"`. The orchestrator is not constrained here:
  it is not supposed to be implementing at all, and `feature-graph` says so in words.

  It is silent when there is no open run, when the run is closed, when the plan lists no
  files, and when `approved_by_human` is not yet true -- in that last case a builder
  should not be running, but that is `--audit`'s finding to report, and denying every
  write instead would deadlock a run whose approval simply has not been recorded yet.

BASH IS COVERED TOO, as of 2026-09-06 -- gap #13. Registering this guard on `Write|Edit`
alone meant a builder writing through `sed -i`, a heredoc, `tee`, `python -c` or a plain
redirect left the approved file set entirely: no denial, no record, no trace. The matcher
is `Write|Edit|Bash` now, and `bash_write_targets.classify` resolves what a command is
about to write. Every target it resolves goes through the SAME `approved_paths` /
`covered` / `_match` / `worktree_context` a `Write` goes through -- there is no second
matcher, because this guard has been fixed twice already for having two that drifted.

  What that coverage is NOT. A command whose write shape cannot be resolved -- a target
  behind an unassigned shell variable, a `$(...)` substitution -- is ALLOWED with a
  warning naming it. So is a write performed by a program the command merely invokes:
  `npm run build`, `make`, `pytest`, `bash script.sh`. Both are deliberate. The
  population here is cooperative, and a guard that denies `pytest`, or denies what it
  merely failed to parse, is switched off within a day and then closes nothing at all.
  The warning is the point in those cases: gap #13's complaint was "no record", and a
  record is what the unresolved branch leaves.

  A Bash write to a temp directory or to `/dev/null` is allowed and not reported. Those
  are sinks, not the tree the plan is about. `bash_write_targets` drops the null sinks
  itself; the temp roots are dropped here, and deliberately NOT added to the approved
  set, so a `Write` tool call to a temp path is denied exactly as it was before.

The escape hatch is `scope_exceptions` in the run's `state.json`, which the orchestrator
owns. Adding a path there is deliberate and auditable; editing `architect.plan` to widen
the file set would be the orchestrator rewriting another node's key, which is exactly
what `written_by` was added to catch.

Exit 0 always. Printing no JSON means "no opinion" and the tool proceeds normally.

It FAILS CLOSED, as of 2026-09-02. The silences above are deliberate and stay allows,
but an unexpected exception emits a deny that names this file as broken, rather than
letting a guard that never ran read as a guard that approved. See `main()`.
"""
import fnmatch
import json
import os
import re
import sys
import traceback

HERE = os.path.dirname(os.path.realpath(__file__))
FLEET = os.path.normpath(os.path.join(HERE, "..", ".."))      # @FLEET@/
UMBRELLA = os.path.dirname(FLEET)                             # the project root
CURRENT = os.path.join(FLEET, ".graph", "CURRENT")
SCHEMA = os.path.join(FLEET, ".graph", "runs", "_schema.json")
# `parked` added to match `show-board.py`. A pointer left on a parked run must no more
# constrain a later builder than a pointer left on a closed one.
CLOSED = ("done", "blocked", "parked")

# How much of a command to quote back in a denial. Enough to recognise it, not enough to
# paste a 40KB heredoc into the builder's context.
COMMAND_ECHO = 400

# What `bash_targets` puts in its second slot when a Bash command was NOT judged against
# the approved set. Both are allow-and-record, and they are two labels rather than one
# boolean because they are two different facts about the command -- and a warning that
# reports the wrong one of them tells the builder to fix something that is not broken.
UNRESOLVED_SHAPE = "shape"      # parsed, but a target is a runtime value
OVER_MAX_COMMAND = "size"       # never parsed: longer than the classifier's cap


_MSYS_DRIVE = re.compile(r"^(?:/cygdrive)?/([A-Za-z])(/|$)")


def _from_msys(path):
    """`/c/dir/x` or `/cygdrive/c/dir/x` -> `c:/dir/x`, on Windows only.

    Git Bash reports its cwd, and a builder types absolute targets, in MSYS spelling.
    Windows `realpath` reads `/c/Users` as rooted-on-the-current-drive, i.e. `C:/c/Users`,
    which matches no approved path. Anything that is not exactly a one-letter drive
    mount (`/cc/x`, `//c/x`, `/zz/x`, a relative path) is returned unchanged. `/mnt/c`
    (WSL) is deliberately not handled.
    """
    if os.name != "nt" or not isinstance(path, str):
        return path
    m = _MSYS_DRIVE.match(path)
    if not m:
        return path
    return "%s:/%s" % (m.group(1), path[m.end():])


def norm(path):
    """Absolute, junction-resolved, forward-slashed, case-folded on Windows.

    An MSYS/Cygwin drive spelling is translated first (`_from_msys`), so every caller --
    Write, Bash, approved paths, transient roots -- sees the same path for the same file.
    """
    path = _from_msys(path)
    try:
        real = os.path.realpath(path)
    except Exception:
        real = path
    real = str(real).replace("\\", "/").rstrip("/")
    return real.lower() if os.name == "nt" else real


def open_run():
    """(state, run_dir) for the run `.graph/CURRENT` points at, or (None, None)."""
    try:
        with open(CURRENT, encoding="utf-8") as fh:
            run_id = fh.read().strip()
    except OSError:
        return None, None
    if not run_id:
        return None, None
    run_dir = os.path.join(FLEET, ".graph", "runs", run_id)
    try:
        with open(os.path.join(run_dir, "state.json"), encoding="utf-8") as fh:
            state = json.load(fh)
    except (OSError, ValueError):
        return None, None
    return (state, run_dir) if isinstance(state, dict) else (None, None)


def schema_placeholders():
    """Every string `_schema.json` uses as untouched template text, as a set.

    A run's `state.json` starts as a copy of the schema, and its placeholders are PROSE
    describing what a key is for. `scope_exceptions`' placeholder contains "Write/Edit",
    so the "a space and no separator means prose" heuristic below reads it as a path:
    it became a bogus approved entry and a bogus `rel_map` root, and the `Approved:` list
    a denied builder was shown carried a paragraph of schema documentation in it (gap
    #14, worked around once by setting that run's `scope_exceptions` to `[]`).

    Identity against the schema is used rather than a smarter prose heuristic because it
    is exact: the question "is this the untouched template?" has a correct answer on disk,
    and `verify-state.py` already answers it the same way for the keys it checks. A
    missing or malformed schema returns an empty set -- the heuristic still runs, and the
    worst case is the pre-existing cosmetic pollution rather than a guard that stops
    working.

    One behaviour change worth stating, because it is a loosening: a gated run whose plan
    has NO real files and whose only entry is that placeholder used to yield a file set of
    one junk path, and so denied every builder write. It now yields no file set at all,
    which `main()` treats as "no opinion" -- the same answer it already gives an ungated
    or unplanned run. A human gate passed over an empty plan is a broken run either way;
    this stops it being reported as a scope violation, which is the wrong diagnosis.
    """
    try:
        with open(SCHEMA, encoding="utf-8") as fh:
            schema = json.load(fh)
    except (OSError, ValueError):
        return set()

    found = set()

    def walk(node):
        if isinstance(node, str):
            text = node.strip()
            if text:
                found.add(text)
        elif isinstance(node, list):
            for item in node:
                walk(item)
        elif isinstance(node, dict):
            for item in node.values():
                walk(item)

    walk(schema)
    return found


def approved_paths(state, run_dir):
    """{normalised path: as the plan wrote it}. The key compares, the value is read.

    Comparison is case-folded on Windows and the display is not, so the denial message
    quotes the plan's own spelling back rather than a lowercased path the human never
    typed.
    """
    allowed = {norm(os.path.join(run_dir, "state.json")):    # nodes must write their key
               "%s/state.json" % os.path.basename(run_dir)}
    rel_map = {}

    plan = (state.get("architect") or {}).get("plan")
    entries = []
    if isinstance(plan, list):
        for slice_ in plan:
            if isinstance(slice_, dict) and isinstance(slice_.get("files"), list):
                entries.extend(f for f in slice_["files"] if isinstance(f, str))

    exceptions = state.get("scope_exceptions")
    if isinstance(exceptions, list):
        entries.extend(f for f in exceptions if isinstance(f, str))

    placeholders = schema_placeholders()

    planned = False
    for entry in entries:
        entry = entry.strip()
        # An untouched `_schema.json` placeholder is template prose, never a path, however
        # many slashes its sentences happen to contain. Exact identity, checked first.
        if entry in placeholders:
            continue
        # A plan whose `files` still reads like prose is not a file set. Anything with a
        # space and no separator is a description, and treating it as a path would let
        # the union match nothing and deny everything.
        if not entry or (" " in entry and "/" not in entry and "\\" not in entry):
            continue

        # A plan entry is a GLOB, not a literal path: an architect writes
        # `myapp/apps/mobile/**` to mean "everything under apps/mobile". Reduce a
        # trailing wildcard segment to the directory it stands for, so the prefix tests
        # below cover it.
        #
        # Without this the normalised entry kept its literal `**`, and since no real file
        # is ever equal to -- or prefixed by -- a path ending in `**`, the guard denied
        # EVERY write under an approved directory. That happened once: three
        # slices of one run had `myapp/apps/mobile/**` as their
        # entire file set, and s1's builder could not create so much as a package.json in
        # a directory the human had explicitly approved. It cost a round trip and a
        # `scope_exceptions` entry that granted nothing the plan had not already granted.
        #
        # This narrows nothing: under the prefix rule `a/b/**` and `a/b` cover exactly the
        # same files. Residual wildcards deeper in the entry (`a/**/*.ts`) survive here and
        # are matched by fnmatch in `_match` instead.
        entry_path = entry.replace("\\", "/").rstrip("/")
        while True:
            head, _, tail = entry_path.rpartition("/")
            if head and tail in ("**", "*"):
                entry_path = head
                continue
            break
        if not entry_path:
            continue

        allowed[norm(entry_path if os.path.isabs(entry_path)
                     else os.path.join(UMBRELLA, entry_path))] = entry

        # Same entry, expressed as (repo root, path within that repo). In a project the
        # repo is the project root, so the whole entry is the path within it. This is what
        # lets a builder in a linked worktree be matched: same repo-relative path, different
        # root. A write that matches no entry here is still judged by the absolute rule above.
        if not os.path.isabs(entry_path):
            root = norm(UMBRELLA)
            rel = entry_path.strip("/")
            rel_map.setdefault(root, {})[rel.lower() if os.name == "nt" else rel] = entry

        planned = True

    return (allowed, rel_map) if planned else (None, None)


def _match(target, approved):
    """One approved entry against one target path.

    Exact hit, or living under an approved directory, or -- when the entry still carries
    a wildcard after `approved_paths` reduced its trailing one -- an fnmatch. Every call
    site goes through here on purpose: the absolute rule and the worktree rule drifted
    apart once already, and a matcher that lives in two places is a matcher that will
    disagree with itself. The Bash branch added in 2026-09-06 resolves paths and then
    asks THIS, for the same reason.
    """
    if target == approved or target.startswith(approved + "/"):
        return True
    if "*" in approved or "?" in approved:
        return fnmatch.fnmatch(target, approved)
    return False


def covered(target, allowed):
    """True if the target is an approved path, or lives under an approved directory."""
    return any(_match(target, a) for a in allowed)


def worktree_context(target):
    """(main repo root, path relative to the worktree root), or (None, None).

    A linked git worktree has a `.git` FILE (not a directory) reading
    `gitdir: <main>/.git/worktrees/<name>`. That is read directly rather than shelling
    out to git: this is a PreToolUse hook, it runs before EVERY write, and it must not
    pay for a subprocess on the hot path.

    Returns (None, None) for a main working tree -- there `.git` is a directory and the
    absolute rule in approved_paths already covers the path correctly.
    """
    directory = os.path.dirname(target)
    for _ in range(64):                      # bounded: never walk forever on a odd path
        dotgit = os.path.join(directory, ".git")
        if os.path.isdir(dotgit):
            return None, None                # main tree; absolute rule handles it
        if os.path.isfile(dotgit):
            try:
                with open(dotgit, encoding="utf-8") as fh:
                    line = fh.read().strip()
            except OSError:
                return None, None
            if not line.startswith("gitdir:"):
                return None, None
            gitdir = norm(line.split(":", 1)[1].strip())
            marker = "/.git/worktrees/"
            if marker not in gitdir:
                return None, None
            root = norm(directory)
            if target != root and not target.startswith(root + "/"):
                return None, None
            return gitdir.split(marker)[0], target[len(root) + 1:]
        parent = os.path.dirname(directory)
        if parent == directory:
            return None, None
        directory = parent
    return None, None


def in_scope(target, allowed, rel_map):
    """Is this normalised absolute path inside the approved set? The only scope answer.

    A `Write`'s `file_path` and each path `bash_write_targets` resolved out of a Bash
    command are judged by exactly this function, so the two tools cannot come to
    different conclusions about the same file.
    """
    if covered(target, allowed):
        return True

    # Diamond mode: the builder is in a linked worktree, so its absolute path is rooted
    # somewhere else entirely and can never equal the plan's project-relative path. Match
    # the repo-relative path against the same repo's approved entries instead. Without
    # this the guard denies EVERY write by EVERY builder in a diamond -- which it did,
    # undetected, from the day it was written until the first run actually fanned out.
    main_root, rel = worktree_context(target)
    if main_root and rel:
        entries = rel_map.get(main_root) or {}
        if any(_match(rel, a) for a in entries):
            return True
    return False


def transient_roots():
    """Temp directories, whose contents are not the tree any plan is about.

    Read from the environment rather than through `tempfile`, which pulls in `shutil` on
    import; this runs before every Bash call and the hot path should not pay for a module
    it needs one string from.
    """
    roots = []
    for name in ("TMPDIR", "TEMP", "TMP"):
        value = os.environ.get(name)
        if value and value.strip():
            roots.append(norm(value.strip()))
    roots.append(norm("/tmp"))
    return roots


def bash_targets(command, cwd):
    """(targets, unresolved) for a Bash command; (None, None) if it cannot write at all.

    `unresolved` is False, `UNRESOLVED_SHAPE` or `OVER_MAX_COMMAND` -- truthy in the two
    cases the caller must allow and record, and specific enough that the record says
    which one happened.

    (None, None) is the pre-filter's answer and it is the reason this function exists
    separately: `has_write_signature` is a regex over the raw string, and returning here
    means the caller never opens `.graph/CURRENT` or a `state.json`. Measured over the
    2089 unique commands this fleet has run, 676 of them -- 32.3% -- take that exit and
    pay no disk I/O at all. The guard now sees every Bash call, so that matters.

    The import is deliberately HERE and not at module top. A top-level `ImportError`
    raises before `main()`'s fail-closed handler can catch it, the hook exits non-zero,
    and the harness reads that as a hook error and lets the write through -- which would
    silently re-open gap #17 in the act of closing gap #13. Imported inside the try, a
    missing or broken classifier denies and says so.

    It sits above the length cap and the pre-filter rather than below them, which means a
    broken classifier denies EVERY builder Bash call and not only write-shaped ones --
    `git status` included. That is deliberate and it is the safe direction, but an
    orchestrator recovering from it should know why it looks that way. Narrowing it would
    mean answering "could this write?" without the classifier, i.e. a second copy of the
    pre-filter living here; this guard has been fixed twice for having two matchers that
    drifted, and a locked-out builder that is told what broke costs a minute where a
    silently unguarded run costs a whole human approval.
    """
    if HERE not in sys.path:
        sys.path.insert(0, HERE)
    from bash_write_targets import MAX_COMMAND, classify, has_write_signature

    # The cap goes BEFORE the pre-filter, in exactly the order `classify` puts it and for
    # the reason it states in words: `has_write_signature` is linear in any real command
    # but quadratic in a run of separator characters, so "a cap checked afterwards bounds
    # nothing at all". Running the regex first put that cost straight back -- a 601KB
    # command shaped `("(" + "a/" * 300) * 1000` spent 24.3s in the pre-filter, against
    # the `timeout: 10` this hook is registered with, while `classify` answered in
    # microseconds because its own cap fired first.
    #
    # `MAX_COMMAND` is IMPORTED, never restated. Two copies of a constant drift, and that
    # is the same class of bug this guard has already been fixed for twice.
    if len(command) > MAX_COMMAND:
        # `classify` would return ([], True) here without reading the string at all, so
        # nothing is known about this command -- not its targets, not even whether it
        # writes. Allow and record, and let the caller say THAT rather than blaming an
        # unresolvable target it never looked for.
        return [], OVER_MAX_COMMAND

    if not has_write_signature(command):
        return None, None

    targets, unresolved = classify(command)

    # A target is returned as the command wrote it, with any `cd` in the same command
    # already applied. Relative means relative to the shell's cwd, which the payload
    # carries; the project root is the fallback, and it is what the Bash tool
    # actually uses in every run this fleet has executed. Only the MSYS spelling of the
    # base is translated before the join (Git Bash sends `/c/dir/...`, which `realpath`
    # would map to `C:/c/dir/...`); the base is NEVER passed through `norm`, because
    # `norm` strips the root separator and a drive-root cwd (`/c/`, `C:\`) would become the
    # drive-RELATIVE `c:`, which resolves against the HOOK's cwd instead of the shell's.
    base = UMBRELLA
    unresolvable_cwd = None
    if isinstance(cwd, str) and cwd.strip():
        base = _from_msys(cwd)
        if not os.path.isabs(base) and os.path.splitdrive(base)[0]:
            # A bare or drive-relative spec (`c:`, `c:foo`): the shell's directory on
            # that drive is unknowable here, so nothing relative can be judged. Fail closed.
            unresolvable_cwd = cwd
        # Any other non-absolute cwd (`/zz/x` on Windows, `relative/dir`) is joined as-is,
        # exactly as before this change.
    skip = transient_roots()

    resolved = []
    for target in targets:
        # Translate before `isabs`: on Python 3.13+ Windows `/c/dir/x` is not absolute,
        # so it would be joined onto the base and keep only the base's drive.
        target = _from_msys(target)
        if unresolvable_cwd is not None and not os.path.isabs(target):
            marker = "<relative target under unresolvable cwd %r>" % unresolvable_cwd
            if marker not in resolved:
                resolved.append(marker)       # matches no approved path, so it is denied
            continue
        absolute = norm(target if os.path.isabs(target) else os.path.join(base, target))
        if any(_match(absolute, root) for root in skip):
            continue                          # a temp file is a sink, not the plan's tree
        if absolute not in resolved:
            resolved.append(absolute)
    return resolved, (UNRESOLVED_SHAPE if unresolved else False)


def deny(reason):
    """Emit the deny decision. The only thing in this file that stops a write."""
    json.dump({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    }, sys.stdout)


def warn(message):
    """Leave a record without stopping anything. Carries NO `permissionDecision`.

    That absence is the whole mechanism: a `PreToolUse` payload with no decision is "no
    opinion", so the tool proceeds. `additionalContext` puts the text in the builder's
    context and `systemMessage` puts it in front of the human, matching what the
    PostToolUse flag hooks in this directory already do.
    """
    json.dump({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "additionalContext": message,
        },
        "systemMessage": message,
    }, sys.stdout)


def decide():
    payload = json.load(sys.stdin)

    if str(payload.get("agent_type") or "").strip() != "builder":
        return

    tool_input = payload.get("tool_input") or {}
    path = tool_input.get("file_path")
    command = tool_input.get("command")

    if path:
        targets, unresolved = [norm(path)], False
    elif isinstance(command, str) and command.strip():
        targets, unresolved = bash_targets(command, payload.get("cwd"))
        if targets is None:
            return          # no write shape; nothing was read from disk to decide it
        if not targets and not unresolved:
            return          # a write shape whose every target is a sink
    else:
        return

    state, run_dir = open_run()
    if state is None:
        return
    if str(state.get("status") or "").strip().lower() in CLOSED:
        return          # a stale pointer must not constrain the next run's builder
    if state.get("approved_by_human") is not True:
        return          # --audit owns that finding; denying here would deadlock the run

    allowed, rel_map = approved_paths(state, run_dir)
    if allowed is None:
        return          # no file set to compare against

    outside = [t for t in targets if not in_scope(t, allowed, rel_map)]
    listed = "\n".join("  - %s" % shown for shown in sorted(allowed.values()))
    run_name = os.path.basename(run_dir)

    if outside:
        # Each branch writes its own complete sentence. They used to share one template
        # ending in "... is not in the file set a human approved", which read correctly
        # for a `Write`'s single path and left the Bash branch dangling that predicate
        # after the echoed command -- so the builder read it as though the COMMAND were
        # the thing not in the file set. A denial nobody can parse is a denial that gets
        # routed around instead of obeyed, which is the failure this whole guard exists
        # to avoid.
        if path:
            headline = ("`%s` is not in the file set a human approved for run `%s`."
                        % (path, run_name))
        else:
            headline = ("This Bash command writes %s that %s not in the file set a "
                        "human approved for run `%s`:\n%s\n\nThe command:\n  %s"
                        % ("a path" if len(outside) == 1 else "paths",
                           "is" if len(outside) == 1 else "are",
                           run_name,
                           "\n".join("  - %s" % t for t in outside),
                           command.strip()[:COMMAND_ECHO]))
        deny(
            "[plan-scope] %s\n\n"
            "Approved:\n%s\n"
            "Do not work around this. Stop, report to the orchestrator what you need "
            "and why the approved set was wrong, and let it decide: either the work "
            "belongs to a different slice, or the orchestrator records the extension "
            "in `scope_exceptions` and in this slice's `deviation_from_approved_plan`. "
            "Silently widening scope after the gate is the failure the gate exists to "
            "prevent." % (headline, listed)
        )
        return

    if unresolved:
        # The fail-OPEN half, and the reason gap #13 is "mostly" closed rather than
        # closed. Something about this command could not be judged, so there is nothing
        # to compare against the approved set. Denying on it would be denying what we
        # failed to understand, which gets a guard routed around; allowing it silently is
        # the exact hole this run was opened to close. So: allow, and leave the record.
        #
        # WHICH failure it was is not decoration. The two get different sentences because
        # only one of them is the builder's to do anything about, and the size case used
        # to be reported as the shape case: a 294KB heredoc to an APPROVED path was told
        # its target could not be resolved and advised to spell the path literally, when
        # the path already was literal and had been resolved perfectly -- the classifier
        # had simply hit its cap and never looked. A boundary that explains itself wrongly
        # is worse than one that says nothing.
        if unresolved == OVER_MAX_COMMAND:
            # No advice to give: nothing was parsed, so nothing is known about this
            # command -- not its targets, not whether it writes at all.
            warn(
                "[plan-scope] This Bash command is longer than the write classifier's "
                "cap (%d characters), so it was NOT parsed and NOT checked against the "
                "file set a human approved for run `%s`. Nothing is known about what it "
                "writes -- including whether it writes at all. Allowed, and recorded "
                "here.\n\n"
                "  %s\n\n"
                "Approved:\n%s\n"
                "Builder: if this command writes outside the approved set, that is still "
                "a scope violation and still yours to stop and report. The cap is "
                "`MAX_COMMAND` in `bash_write_targets.py`; a command this long is "
                "usually a large heredoc, and writing the file with the `Write` tool "
                "instead puts it back inside the guard."
                % (len(command), run_name, command.strip()[:COMMAND_ECHO], listed)
            )
        else:
            # Parsed, but a target is a runtime value -- an unassigned variable, a
            # substitution, operands arriving on stdin. Measured at 2.63% of the corpus.
            warn(
                "[plan-scope] This Bash command writes something, and the target could "
                "not be resolved from the command string -- so it was NOT checked "
                "against the file set a human approved for run `%s`. Allowed, and "
                "recorded here.\n\n"
                "  %s\n\n"
                "Approved:\n%s\n"
                "Builder: if that write lands outside the approved set, it is still a "
                "scope violation and still yours to stop and report. Spelling the path "
                "literally instead of through a variable lets the guard do this for you."
                % (run_name, command.strip()[:COMMAND_ECHO], listed)
            )


def main():
    """Fail CLOSED, and only for builders.

    Everything `decide()` returns early on is a deliberate silence and stays an allow.
    An *unexpected* exception is different: it means the guard did not run, and a guard
    that cannot run must not pretend it approved something. Before 2026-09-02 a crash
    here was indistinguishable from "no opinion" -- `main()` swallowed every exception
    and `settings.json` wrapped the call in `2>/dev/null || true` -- so a syntax error
    in this file did not block builders, it silently stopped guarding them, voiding the
    human gate for a whole run with nothing to show for it afterwards.

    The two failure directions are not symmetric. Failing closed blocks ONLY builders
    (the `agent_type` check above exempts the orchestrator and every other node), says
    exactly what broke, and is fixed in a minute. Failing open silently voids a human
    approval and leaves no trace. So: deny, loudly, naming this file as the problem.

    This is also why `bash_write_targets` is imported inside `decide()` rather than at
    module top: an import that fails up there is not caught here at all.

    A malformed payload is the one case still treated as an allow: without a parseable
    payload there is no `agent_type`, so the deny could not be scoped to builders and
    would block every agent in the session. That is the harness's contract to keep, not
    this guard's to enforce.
    """
    try:
        decide()
    except json.JSONDecodeError:
        return
    except Exception:
        deny(
            "[plan-scope] THE SCOPE GUARD ITSELF IS BROKEN -- this is not a scope "
            "violation.\n\n%s\n"
            "`@FLEET@/.claude/hooks/guard-builder-scope.py` raised while deciding, "
            "so the approved file set was never checked. This denial is deliberate: the "
            "guard fails closed rather than silently approving writes nobody verified.\n"
            "Builder: stop and report this to the orchestrator verbatim. Do not retry and "
            "do not route around it with Bash.\n"
            "Orchestrator: fix the hook before resuming the run." % traceback.format_exc(limit=4).strip()
        )


main()
