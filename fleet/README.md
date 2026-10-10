# fleet/ — vendored orchestration. Do not edit by hand.

This directory holds Orchestra's copy of the agent orchestration model: agent definitions,
skills, the run-state schema, and the scripts that verify and render run state.

It is **vendored**. Its upstream is the `graph_agents` fleet, where agents are tuned and
tested. Changes arrive here only through a one-way sync run from upstream, and each sync
records the upstream commit it came from in `fleet/SOURCE` (written by the sync, not by hand).

Rules:

- **Never hand-edit anything under `fleet/`.** The next sync overwrites it. Fix it upstream,
  then sync.
- **Orchestra builds and runs against this copy only.** Nothing in this repo may read,
  import or resolve a path outside the repo to get fleet files. If the upstream disappeared
  tomorrow, Orchestra must still build, test and ship.
- **A sync is a normal change.** Review the diff, commit it, and note the upstream commit
  in the message.

This README is the one file here the sync does not manage; everything else under `fleet/`
is listed, with its hash, in `SOURCE`.

What ships: the six agents (scout, architect, builder, reviewer, integrator, ops), the
`feature-graph`, `close-run` and `rlm` skills, `GRAPH.md`, the hooks (plan-scope guard,
activity recorder, board, state-gap check) and the run-state scripts. Paths to the fleet
appear as the `@FLEET@` placeholder, which Orchestra fills in when it installs the fleet
into a user's project. This directory is a template: nothing runs in place here, so no run
state, `__pycache__` or other generated files belong in it (the sync refuses on extras).
