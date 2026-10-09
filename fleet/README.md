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

Empty until the first sync. The sync tool is being built separately, upstream.
