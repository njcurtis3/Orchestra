#!/usr/bin/env python
"""Verify that a node actually wrote its key in a run's state.json.

    python @FLEET@/.graph/verify-state.py <run-id> <key> [<key> ...]
    python @FLEET@/.graph/verify-state.py --audit <run-id>

Exit 0 only if EVERY named key exists and is non-empty. Anything else exits 1 and
names the offending key on stderr. Dotted keys walk nested objects: `reviews.s1`,
`builders.s2`.

`--audit` is the hook-firable half, added 2026-08-26. The named-key mode above can
only run when someone already knows WHICH node just finished, so it can only ever be
called by hand. A PostToolUse hook sees a file path and nothing else, so it cannot ask
that question at all. `--audit` asks the one a lone state.json can answer instead:
given everything written so far, did the graph's EDGES hold? Builders before the human
gate, a review with no build behind it, a fan-in over a slice whose latest review is not
a PASS, a run closed with a slice still unreviewed, a key filled in around template text
still left in place. It names no node and needs no argument beyond the run.

The two modes are complementary, not redundant: `--audit` never reports a key as
merely unwritten mid-run (that is what a run in progress looks like), and the named-key
mode never notices ordering. Keep both.

"Non-empty" is the whole point. A node that wrote `{}`, `[]`, `""`, or a dict whose
values are all empty did NOT do its job. Presence alone is not evidence of work.

Neither is a non-empty PLACEHOLDER. `feature-graph` step 1 opens a run by copying
`_schema.json`, whose values are all descriptive strings, so an untouched key is
non-empty and would sail through. A value byte-identical to the one in the template
is therefore treated as unwritten too. Without this the check is green in exactly
the situation it exists to catch: a run where no node has executed.

This is a checker: pure stdlib, no network, and it never writes.
"""
import json, os, sys

RUNS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "runs")
SCHEMA = os.path.join(RUNS, "_schema.json")


def is_empty(value):
    """True when the value carries no work. Recurses: all-empty children == empty."""
    if value is None:
        return True
    if isinstance(value, str):
        return value.strip() == ""
    if isinstance(value, dict):
        return all(is_empty(v) for v in value.values())
    if isinstance(value, list):
        return all(is_empty(v) for v in value)
    return False  # numbers and booleans are real values


def state_path(run_id):
    """A run id, or a direct path to a state.json."""
    if run_id.endswith(".json"):
        return run_id
    return os.path.join(RUNS, run_id, "state.json")


def load(run_id):
    path = state_path(run_id)
    if not os.path.isfile(path):
        die("no such run: %s (looked for %s)" % (run_id, path))
    try:
        with open(path, encoding="utf-8") as fh:
            return json.load(fh), path
    except ValueError as exc:
        # UnicodeDecodeError and JSONDecodeError are both ValueError. PowerShell
        # redirection writes UTF-16-with-BOM, which is the former, not the latter.
        die("malformed JSON in %s: %s" % (path, exc))
    except OSError as exc:
        die("cannot read %s: %s" % (path, exc))


def load_template():
    """The run template. A missing or broken one degrades to the empty-check alone."""
    try:
        with open(SCHEMA, encoding="utf-8") as fh:
            template = json.load(fh)
    except (OSError, ValueError) as exc:
        sys.stderr.write("verify-state: WARNING: cannot read %s (%s) -- checking for "
                         "empty values only, untouched placeholders will pass\n"
                         % (SCHEMA, exc))
        return {}
    if not isinstance(template, dict):
        sys.stderr.write("verify-state: WARNING: %s is not a JSON object -- checking for "
                         "empty values only, untouched placeholders will pass\n" % SCHEMA)
        return {}
    return template


def resolve(state, key):
    """Walk a dotted key. Returns (found, value)."""
    node = state
    for part in key.split("."):
        if not isinstance(node, dict) or part not in node:
            return False, None
        node = node[part]
    return True, node


def die(message):
    sys.stderr.write("verify-state: %s\n" % message)
    raise SystemExit(1)


# ------------------------------------------------------- review attempts (2026-09-05)
#
# A rejected slice keeps attempt 1's REJECT at the TOP of `reviews.<slice>` and nests the
# re-review as `attempt_2` (`attempt_3`, ...). Every attempt survives; the LATEST one
# supplies the verdict. Two reviewers invented that shape independently and nothing in the
# fleet read it, so a slice that was rejected and then fixed reported as failed -- which is
# why `close-run.py` could not close such a run and why the audit had been
# misreporting it since the day it closed.
#
# The rule lives HERE, once, because `close-run.py` and `brief.py` already import this
# module: four readers with four rules is how this defect comes back. `resolve()` above is
# deliberately NOT taught about attempts -- it is a dumb dotted-path walker, and a magic
# resolve would silently change every unrelated dotted read in three scripts.
#
# The fleet board's `reviewAttempts`/`finalVerdict`/`everRejected` is the reference this
# mirrors, read and copied, never imported.

# The board's walk is `for (var i = 2; i < 10; i++)`, so it stops after attempt_9. This
# matches that bound EXACTLY on purpose: two readers with different caps disagreeing about
# a verdict, in a place nobody would look, is the precise divergence this rule exists to
# prevent. Where they would silently differ -- attempt_10 and beyond -- this side is loud
# instead of quietly authoritative. See `over_cap_attempts()`.
ATTEMPT_CAP = 9

_WARNED = set()


def _norm(verdict):
    """A verdict as the gates compare it. Strip and upper, and nothing else.

    No mapping and no validation: an unrecognised verdict must reach the reader verbatim,
    because "reviews.s1 is PASS|REJECT" (the untouched template) is a finding, not a typo
    to be cleaned up on the way past.
    """
    return str(verdict if verdict is not None else "").strip().upper()


def _attempt(obj, number, key):
    """One attempt, flattened to the fields every reader needs. Mirrors the board's."""
    return {"verdict": _norm(obj.get("verdict")),
            "attempt": obj.get("attempt") or number,
            "summary": obj.get("summary"),
            "findings": obj.get("findings"),
            "key": key}


def over_cap_attempts(review):
    """`attempt_N` keys present beyond ATTEMPT_CAP, in order. Normally empty.

    Past the cap the fleet and the board WOULD disagree, so this is surfaced rather than
    resolved: `review_attempts()` warns on stderr and `audit()` reports it as a violation.
    Returning a stale verdict silently is the one outcome that is not allowed here.
    """
    if not isinstance(review, dict):
        return []
    over = []
    for key in review:
        if not key.startswith("attempt_"):
            continue
        try:
            number = int(key[len("attempt_"):])
        except ValueError:
            continue
        if number > ATTEMPT_CAP:
            over.append((number, key))
    return [key for _, key in sorted(over)]


def review_attempts(review):
    """Every attempt in `reviews.<slice>`, in order. The top level IS attempt 1.

    Walks `attempt_2`, `attempt_3`, ... and stops at the FIRST GAP, so a lone `attempt_3`
    with no `attempt_2` does not silently become the verdict -- a missing attempt means the
    numbering is not what the reader thinks it is, and guessing past it is how a verdict
    nobody wrote gets applied.

    One documented divergence from the board, which breaks on `!a`: an EMPTY attempt dict
    stops the walk here too. `{}` is truthy in JS, but in this file empty has always meant
    "the node did not write it" (`is_empty`), and an empty attempt is not a re-review.
    """
    if not isinstance(review, dict):
        return []
    out = [_attempt(review, 1, "")]
    for i in range(2, ATTEMPT_CAP + 1):
        nested = review.get("attempt_%d" % i)
        if not isinstance(nested, dict) or is_empty(nested):
            break
        out.append(_attempt(nested, i, "attempt_%d" % i))
    for key in over_cap_attempts(review):
        message = ("verify-state: WARNING: %s is past the attempt_%d cap this fleet and "
                   "the board both stop at -- it is NOT resolved, and the two readers now "
                   "disagree about this slice's verdict\n" % (key, ATTEMPT_CAP))
        if message not in _WARNED:
            _WARNED.add(message)
            sys.stderr.write(message)
    return out


def final_verdict(review):
    """The LAST attempt, or None. This is what decides pass/fail everywhere."""
    attempts = review_attempts(review)
    return attempts[-1] if attempts else None


def ever_rejected(review):
    """Did ANY attempt REJECT.

    DISPLAY ONLY, forever. It must never gate anything: the fleet is built to loop, so a
    check that blocked on "ever REJECTed" would fire on every run that did the normal thing
    -- which is exactly the 2026-08-26 breakage recorded at `real_slices()`, where no
    diamond could close. It exists so a rejection stays VISIBLE after it is fixed, which is
    the regression the latest-attempt rule would otherwise cause.
    """
    return any(a["verdict"] == "REJECT" for a in review_attempts(review))


def slice_verdict(state, sid):
    """The verdict of `reviews.<sid>`'s latest attempt, normalised. "" when unwritten."""
    _, review = resolve(state, "reviews.%s" % sid)
    final = final_verdict(review)
    return final["verdict"] if final else ""


# ---------------------------------------------------------------- audit mode

# `_schema.json` describes ONE generic slice, `s1`. A real run has s1..sN, all of the
# same shape, so placeholder identity for `builders.s7` is judged against `builders.s1`.
GENERIC = {"builders": "s1", "reviews": "s1"}

# Which node is allowed to write which key. `written_by` (added 2026-08-26) is the only
# authorship this file has ever recorded; before it, "never rewrite another node's key"
# was unverifiable by construction -- an orchestrator hand-writing all six keys read
# exactly like six nodes doing their jobs.
OWNER = {"scout": "scout", "architect": "architect", "integrator": "integrator",
         "ops": "ops", "builders": "builder", "reviews": "reviewer"}


def owner_of(key):
    """The node that must have written this key, or None if nobody owns it."""
    head = key.split(".")[0]
    return OWNER.get(head)


def template_slot(template, key):
    """The template value a real key should be compared against. (found, value)."""
    parts = key.split(".")
    if len(parts) == 2 and parts[0] in GENERIC:
        parts[1] = GENERIC[parts[0]]
    return resolve(template, ".".join(parts))


def is_untouched(value, tpl):
    """True when nothing in `value` is evidence that a node wrote it.

    Byte-equality against the template is not enough, and 2026-08-26 proved it: adding
    `written_by` to `_schema.json` made every OLD run's untouched `integrator` key stop
    matching the new template, so the audit called it written and then complained about
    the template text inside it. A template that may grow cannot be compared whole.

    So: evidence of writing is a string leaf that differs from the template's, a key the
    template does not have, or content in a list the template left empty. Numbers and
    booleans are no evidence either way -- that is blind spot (5), still open.
    """
    if isinstance(value, dict):
        if not isinstance(tpl, dict):
            return False
        return all(k in tpl and is_untouched(v, tpl[k]) for k, v in value.items())
    if isinstance(value, list):
        if not isinstance(tpl, list):
            return False
        if not tpl:
            return not value            # no exemplar to match, so any content is writing
        return all(is_untouched(item, tpl[0]) for item in value)
    if isinstance(value, str):
        return isinstance(tpl, str) and value == tpl
    return True


def unwritten(state, template, key):
    """True when a key is absent, empty, or still nothing but template text."""
    found, value = resolve(state, key)
    if not found or is_empty(value):
        return True
    placeheld, placeholder = template_slot(template, key)
    return bool(placeheld and is_untouched(value, placeholder))


def _leftover_placeholders(value, tpl, path, out):
    """Template STRINGS still sitting inside an otherwise-written key.

    Strings only, on purpose. A real `attempt: 1` or `gated: true` is byte-identical to
    the template's and always will be, so comparing numbers or booleans would report
    correct work as unfinished. `slice` is excluded for the same reason: the architect
    legitimately writes `"slice": "s1"`, which is exactly what the template says.
    """
    if isinstance(value, dict) and isinstance(tpl, dict):
        for k, v in value.items():
            # `written_by` is excluded because the authorship check below judges it
            # strictly harder -- it must equal the OWNING NODE's name, so leaving the
            # template's description there is already reported, once, as a wrong owner.
            if k in tpl and k not in ("slice", "$comment", "written_by"):
                _leftover_placeholders(v, tpl[k], "%s.%s" % (path, k) if path else k, out)
    elif isinstance(value, list) and isinstance(tpl, list) and tpl:
        for i, item in enumerate(value):
            _leftover_placeholders(item, tpl[0], "%s[%d]" % (path, i), out)
    elif isinstance(value, str) and isinstance(tpl, str):
        if value.strip() and value == tpl:
            out.append(path)


def slices(state):
    """Every slice id this run knows about, from the plan and from what nodes wrote."""
    found = []
    _, plan = resolve(state, "architect.plan")
    if isinstance(plan, list):
        for entry in plan:
            if isinstance(entry, dict) and isinstance(entry.get("slice"), str):
                found.append(entry["slice"])
    for group in ("builders", "reviews"):
        _, node = resolve(state, group)
        if isinstance(node, dict):
            found.extend(k for k in node)
    seen, ordered = set(), []
    for s in found:
        if s not in seen:
            seen.add(s)
            ordered.append(s)
    return ordered


def real_slices(state, template):
    """`slices()` minus ids that are nothing but `_schema.json` template noise.

    `slices()` sweeps every key under `builders`/`reviews`, so an untouched template
    example -- `s1` in the shipped schema -- is indistinguishable from a real slice and
    carries the literal verdict "PASS|REJECT". That made the fan-in check below fire on
    EVERY run that reached an integrator: no diamond could ever close with a green audit.
    Found by an early run, the first to reach fan-in.

    A slice is real if the architect PLANNED it, or if some node actually WROTE its
    builders/reviews key. Both halves are load-bearing and neither may be dropped:

      - planned-but-unwritten must stay in, or `status: done` with a slice never built
        stops being detectable -- the check that catches a dropped slice.
      - written-but-unplanned must stay in, or a slice a node invented off-plan goes
        unaudited. That is exactly `builders.closing_fix` in an early run,
        the defect this audit was built to catch.

    Only the intersection of neither -- unplanned AND unwritten -- is template noise.
    """
    planned = set()
    _, plan = resolve(state, "architect.plan")
    if isinstance(plan, list):
        planned = {e["slice"] for e in plan
                   if isinstance(e, dict) and isinstance(e.get("slice"), str)}
    return [s for s in slices(state)
            if s in planned
            or not unwritten(state, template, "builders.%s" % s)
            or not unwritten(state, template, "reviews.%s" % s)]


def audit(state, template):
    """Edge violations visible in the state file alone. Empty list == nothing wrong.

    Only VIOLATIONS. A half-filled run is what work in progress looks like, so a key
    that is merely not written yet is never reported -- a hook that cried at every
    intermediate write would be turned off within a day, and then it checks nothing.
    """
    problems = []
    approved = resolve(state, "approved_by_human")[1] is True
    status = str(resolve(state, "status")[1] or "").strip().lower()
    known = real_slices(state, template)

    for s in known:
        built = not unwritten(state, template, "builders.%s" % s)
        reviewed = not unwritten(state, template, "reviews.%s" % s)
        verdict = slice_verdict(state, s)

        # The human gate is the one edge in this graph that exists to be blocking.
        if built and not approved:
            problems.append(
                "builders.%s is written but approved_by_human is not true -- the human "
                "gate (feature-graph step 4) was skipped or is recorded wrongly" % s)
        if reviewed and not built:
            problems.append(
                "reviews.%s is written but builders.%s is not -- a review with no build "
                "behind it reviews nothing" % (s, s))
        if status == "done" and not built:
            problems.append(
                "run is status 'done' but builders.%s was never written -- a planned "
                "slice was dropped, or the run closed early" % s)
        elif status == "done" and verdict != "PASS":
            problems.append(
                "run is status 'done' but reviews.%s is %s -- only PASS closes a slice"
                % (s, verdict or "unwritten"))

        # Past the cap the fleet stops resolving and the board keeps its own answer, so
        # the disagreement is reported to a human instead of one of the two winning.
        for over in over_cap_attempts(resolve(state, "reviews.%s" % s)[1]):
            problems.append(
                "reviews.%s.%s is past the attempt_%d cap that this audit and the board "
                "both stop at -- its verdict is NOT resolved here, and the board and the "
                "fleet now disagree about reviews.%s" % (s, over, ATTEMPT_CAP, s))

    # This check NARROWED on 2026-09-05, and the narrowing was approved at a human gate
    # rather than assumed. It used to mean "no slice ever REJECTed"; it now means "no
    # slice's LATEST attempt is REJECT". `state.json` carries no ordering between the
    # integrator's write and a re-review's, so "merged before the re-review landed" was
    # never detectable from this file by any rule. The alternative -- blocking on
    # `ever_rejected` -- would fire on every run that ever looped, which is the intended
    # workflow, and would repeat the breakage recorded at `real_slices()` above. A slice
    # still sitting at REJECT still blocks fan-in.
    if not unwritten(state, template, "integrator"):
        for s in known:
            v = slice_verdict(state, s)
            if v != "PASS":
                problems.append(
                    "integrator is written but reviews.%s is %s -- fan-in (step 6) runs "
                    "only when EVERY slice has passed" % (s, v or "unwritten"))

    # -- authorship. Every written node key must name the node that wrote it.
    node_keys = ["scout", "architect", "integrator", "ops"] + \
                ["builders.%s" % s for s in known] + ["reviews.%s" % s for s in known]
    written = [k for k in node_keys if not unwritten(state, template, k)]
    stamped = [k for k in written
               if isinstance(resolve(state, "%s.written_by" % k)[1], str)
               and resolve(state, "%s.written_by" % k)[1].strip()]

    if written and not stamped:
        # A run from before `written_by` existed, or one where nobody stamped anything.
        # ONE line, not one per key: the finding is "this run has no authorship at all",
        # and repeating it six times would bury the violations that differ.
        problems.append(
            "no key in this run is authorship-stamped -- `written_by` was added to "
            "_schema.json on 2026-08-26; runs opened before that are unverifiable on "
            "the never-rewrite-another-node's-key contract and stay that way")
    else:
        for key in written:
            expected = owner_of(key)
            actual = resolve(state, "%s.written_by" % key)[1]
            if key not in stamped:
                problems.append(
                    "%s is written but has no `written_by` -- an unstamped key in a run "
                    "that stamps the others is the shape of a key written by the wrong "
                    "node" % key)
            elif expected and str(actual).strip() != expected:
                problems.append(
                    "%s.written_by is %r but only `%s` may write that key -- this is the "
                    "contract violation `state.json` exists to prevent"
                    % (key, str(actual).strip(), expected))

    # -- a scope exception is a widening of what the human approved. It is allowed, but
    # it must be paired with the slice's own account of why the approved set was wrong.
    _, exceptions = resolve(state, "scope_exceptions")
    if isinstance(exceptions, list) and not is_empty(exceptions) \
            and not is_untouched(exceptions, resolve(template, "scope_exceptions")[1]):
        explained = any(
            not is_empty(resolve(state, "builders.%s.deviation_from_approved_plan" % s)[1])
            for s in known)
        if not explained:
            problems.append(
                "scope_exceptions widens the approved file set but no slice records "
                "`deviation_from_approved_plan` -- an unexplained exception is scope "
                "creep with the guard switched off")

    _, ops_actions = resolve(state, "ops.actions")
    if not is_empty(ops_actions) and not approved:
        problems.append(
            "ops.actions is non-empty but approved_by_human is not true -- ops always "
            "runs behind its own gate (feature-graph step 7)")

    # Blind spot (6) from the list below: a key filled in AROUND template text.
    for key in ["scout", "architect", "integrator", "ops"] + \
               ["builders.%s" % s for s in known] + ["reviews.%s" % s for s in known]:
        if unwritten(state, template, key):
            continue
        found, value = resolve(state, key)
        placeheld, tpl = template_slot(template, key)
        if not (found and placeheld):
            continue
        leftovers = []
        _leftover_placeholders(value, tpl, key, leftovers)
        for path in leftovers:
            problems.append(
                "%s is still verbatim template text inside an otherwise-written key -- "
                "the node filled in around it" % path)

    for key in ("run_id", "goal", "app", "status"):
        if unwritten(state, template, key):
            problems.append("%s is unwritten or still the template's -- step 1 fills "
                            "these when the run is opened" % key)

    return problems


def main(argv):
    if argv and argv[0] == "--audit":
        if len(argv) != 2:
            die("usage: verify-state.py --audit <run-id>")
        run_id = argv[1]
        state, path = load(run_id)
        if not isinstance(state, dict):
            die("%s is not a JSON object" % path)
        problems = audit(state, load_template())
        if problems:
            for line in problems:
                sys.stderr.write("verify-state: %s\n" % line)
            sys.stderr.write("verify-state: %s -- %d edge violation(s)\n"
                             % (run_id, len(problems)))
            return 1
        print("verify-state: %s -- audit clean" % run_id)
        return 0

    if len(argv) < 2:
        die("usage: verify-state.py <run-id> <key> [<key> ...]\n"
            "       verify-state.py --audit <run-id>")

    run_id, keys = argv[0], argv[1:]
    state, path = load(run_id)
    if not isinstance(state, dict):
        die("%s is not a JSON object" % path)

    template = load_template()

    failures = []
    for key in keys:
        found, value = resolve(state, key)
        placeheld, placeholder = resolve(template, key)
        if not found:
            failures.append("%s: key not present" % key)
        elif is_empty(value):
            failures.append("%s: present but empty -- the node did not write it" % key)
        elif placeheld and is_untouched(value, placeholder):
            # Not `value == placeholder`: the template can gain fields (it gained
            # `written_by` on 2026-08-26), and whole-value equality silently passes
            # every key copied from an older version of it.
            failures.append("%s: still the schema placeholder -- the node did not "
                            "write it" % key)

    if failures:
        for line in failures:
            sys.stderr.write("verify-state: %s\n" % line)
        sys.stderr.write("verify-state: %s -- %d of %d key(s) FAILED\n"
                         % (run_id, len(failures), len(keys)))
        return 1

    print("verify-state: %s -- %d key(s) OK: %s" % (run_id, len(keys), ", ".join(keys)))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
