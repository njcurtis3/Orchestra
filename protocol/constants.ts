// Protocol v1 constants. fixtures/constants.json repeats them and both test suites assert
// the two agree, so a change here that skips the JSON (or the Rust copy) fails loudly.

export const PROTOCOL_NAME = "orchestra";
export const PROTOCOL_VERSION = 1;
export const SUPPORTED_VERSIONS: readonly number[] = [1];

// One text field a client sends, in UTF-8 bytes. Also caps every client-sent string.
export const MAX_TEXT_BYTES = 32768;
// One decoded JSON message, in UTF-8 bytes. Splitting it across Noise frames is P3's job.
export const MAX_MESSAGE_BYTES = 4194304;
// call.id: client-generated, non-empty.
export const MAX_ID_BYTES = 128;

export const SCOPES = ["read", "answer", "control", "land"] as const;
export type Scope = (typeof SCOPES)[number];

// The Tauri command names verbatim. pick_project is host-local and is not a wire method.
export const METHODS = [
  "list_projects",
  "list_sessions",
  "list_inbox",
  "get_transcript",
  "list_tasks",
  "get_task_diff",
  "storage_status",
  "start_session",
  "stop_session",
  "answer",
  "create_task",
  "cancel_task",
  "land_task",
  "discard_task",
  "retry_cleanup",
] as const;
export type Method = (typeof METHODS)[number];

// The scope table of README "Authorisation" as data. Not enforced in P2.
export const METHOD_SCOPE: Readonly<Record<Method, Scope>> = {
  list_projects: "read",
  list_sessions: "read",
  list_inbox: "read",
  get_transcript: "read",
  list_tasks: "read",
  get_task_diff: "read",
  storage_status: "read",
  answer: "answer",
  start_session: "control",
  stop_session: "control",
  create_task: "control",
  cancel_task: "control",
  retry_cleanup: "control",
  land_task: "land",
  discard_task: "land",
};

// Tauri event names verbatim, plus the reserved `notice` (payload typed by H2).
export const EVENT_NAMES = [
  "session-updated",
  "inbox-updated",
  "transcript-appended",
  "task-updated",
  "notice",
] as const;
export type EventName = (typeof EVENT_NAMES)[number];

export const ERROR_CODES = [
  "unsupported_version",
  "unknown_type",
  "bad_request",
  "forbidden",
  "revoked",
  "already_answered",
  "failed",
] as const;
export type ErrorCode = (typeof ERROR_CODES)[number];
