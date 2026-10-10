# stream-json control protocol: spike results

Captured empirically against `claude` 2.1.296 (Windows, logged in, model `haiku`)
on 2026-10-09, run from a scratch directory outside any repo. Four short sessions,
each driven by a small Python host that spoke JSONL over the child's stdin/stdout.

## Fixture format

Each line is `{"_dir": "out"|"in"|"note", ...}`. `out` = host -> claude stdin
(`msg` holds the message), `in` = claude stdout -> host (`msg`), `note` = harness
observation (`alive_after_final_result`, `exit`). Strip `_dir` to get the wire message.

| file | scenario |
|---|---|
| `permission_allow.jsonl` | `touch probe.txt` -> `can_use_tool` -> host replies allow |
| `permission_deny.jsonl` | same, host replies deny |
| `ask_user_question.jsonl` | model calls AskUserQuestion; host answers with `answers` |
| `multi_turn.jsonl` | two user messages on one process |
| `resume_unknown_id.jsonl` | `--resume` with an id claude does not know -> an error result, exit 1 |

## Redaction

Applied mechanically to every string before the files entered the repo:
- username, home dir, cwd, scratchpad and memory paths -> `C:/Users/USER/proj` (or `REDACTED`)
- every UUID (session_id, uuid, request_id, hook_id) -> `00000000-0000-0000-0000-0000000000NN`,
  numbered per file in order of first appearance so correlation still works
- `msg_*`, `toolu_*`, `req_*` ids -> `msg_REDACTED` / `toolu_REDACTED` / `req_REDACTED`
- `apiKeySource`, `memory_paths`, `messaging_socket_path`, `powershell_path` -> `REDACTED`
- thinking `signature` -> `REDACTED_SIGNATURE`
- `rate_limit_event` `resetsAt`/`utilization` -> 0, `overageDisabledReason` -> `REDACTED`
- `init` lists describing the account/install (`mcp_servers`, `slash_commands`,
  `terminal_slash_commands`, `skills`, `plugins`, `agents`, `capabilities`) -> `"REDACTED_LIST"`
- `tools` in every `init`: all `mcp__*` entries removed (they enumerate the owner's claude.ai
  connectors); only built-in tool names remain
- harness `stderr` text dropped

Not redacted: model name, `output_style`, built-in names in `tools`, token/cost numbers, timestamps. A few
path-valued fields still carry the placeholder as a prefix (e.g. `C:/Users/USER/proj\probe.txt`).
No email or org/account id appeared in any capture.

## Findings

1. **Permission channel: `--permission-prompt-tool stdio`.** With it, prompts that would
   normally be asked are emitted on stdout. **No `initialize` control_request is needed**:
   the host sends a plain user message first and the session proceeds. Without a
   prompting tool and with `--permission-prompts none` nothing is emitted (see 6).
2. **Inbound prompt** (stdout):
   `{"type":"control_request","request_id":"<id>","request":{"subtype":"can_use_tool","tool_name":"Bash","display_name":"Bash","input":{...},"description":"...","permission_suggestions":[{type:addRules|addDirectories|setMode,...}],"blocked_path":"...","tool_use_id":"..."}}`.
   **Reply** (stdin), one JSON line:
   `{"type":"control_response","response":{"subtype":"success","request_id":"<same id>","response":{"behavior":"allow","updatedInput":<input object>}}}`
   or `...,"response":{"behavior":"deny","message":"<text>"}}}`. Deny makes the tool_result
   `{"content":"<message>","is_error":true}` and the model reports the denial; the command is not run.
   `echo hi` is auto-allowed (read-only) and never prompts; use a write like `touch` to trigger.
   The `request_id` is a UUID and the reply must echo it.
3. **AskUserQuestion arrives the same way**: `can_use_tool`, `tool_name":"AskUserQuestion"`,
   `input.questions[]` = `{question, header, options:[{label,description}], multiSelect}`,
   plus `requires_user_interaction:true` and `tool_use_id` (no permission_suggestions).
   Reply `behavior:"allow"` with `updatedInput` = the original input **plus**
   `"answers":{"<question text>":"<chosen option label>"}`. The model then received
   `Your questions have been answered: "Which color?"="Red"` and used it.
4. **Multi-turn works.** After a `result` message the process stays alive
   (`alive_after_final_result: true`); a second `{"type":"user",...}` line on stdin starts a new
   turn with the same session_id (a fresh `system/init` is emitted per turn). Closing stdin
   ends the process with exit 0.
5. **Hook events:** with `--include-hook-events` and no hooks configured, only
   `system/hook_started` + `system/hook_response` for `SessionStart:startup` appear, once per
   process (not per turn). No PreToolUse/PostToolUse/Stop/SessionEnd events were emitted, so
   the stream itself (assistant / user tool_result / result / control_request) is the status source.
   `SessionEnd` did not appear on stdin close.
6. **`--permission-prompts none`**: anything needing approval is denied automatically with no
   control_request; a `system/permission_denied` message and an error tool_result
   ("this session has no approval surface") are emitted instead. `host` is the default meaning
   "SDK host or --permission-prompt-tool answers". (Observed in a one-off run; not a committed fixture.)

Other stdout message types seen: `system/init` (carries `session_id`, `cwd`, `permissionMode`),
`assistant`, `user` (tool_result), `rate_limit_event`, `result` (`subtype:"success"`, `is_error`,
`session_id`, `num_turns`, `permission_denials`).

## Fixed spawn args for s2

```
claude -p --input-format stream-json --output-format stream-json --verbose \
  --include-hook-events --permission-prompt-tool stdio [--model <m>] [--max-turns <n>]
```

cwd = the project directory; `--permission-prompts` left at its default (`host`).
Write one JSON user message per line to stdin
(`{"type":"user","message":{"role":"user","content":"<text>"}}`); read stdout line by line;
answer every `control_request` with `can_use_tool` using the reply shape in 2/3, keyed by `request_id`.

## Resume (D2)

Probed by hand against `claude` 2.1.296 (Windows, model `haiku`) with the fixed args plus
`--resume <session id>`:

- Resuming a known id emits `system/init` with the **same** `session_id`, accepts a new user
  line, returns a `result` with that id, and exits 0 when stdin closes.
- A `can_use_tool` after resume works as before; an allow reply is honoured.
- A request left open when the old process was killed is **not** re-emitted on resume.
- Resuming an unknown id returns a `result` with `subtype: "error_during_execution"` and the
  id echoed, then exits 1. `resume_unknown_id.jsonl` is **reconstructed** from that
  observation, not a verbatim capture: `is_error` was not recorded in the probe, so the
  fixture sets it to `true` (the `result` parser reads a missing `is_error` as `false`), and
  `duration_ms`, `num_turns` and the `errors` text are illustrative. Ids are placeholders.
- Not tested: resuming from a different working directory than the original session.

VERDICT: CONFIRMED
