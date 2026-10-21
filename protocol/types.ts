// Protocol v1 wire types. Source of truth; the Rust copy is orchestra_core::wire.
// Erasable TypeScript only (no enum, no namespace), so tsc, Vite, Metro and Babel all
// compile this directory as plain source.
import type { Method, Scope } from "./constants";

// A closed set on the host and an open one on the client: a value this build does not know
// is kept as the string it arrived as and shown as plain text (README "Versioning").
export type Open<T extends string> = T | (string & {});

// ---- Payload types: the local IPC types, field for field (the mirror rule) ----

export type Project = { id: string; name: string };

export type SessionStatus = Open<
  | "starting"
  | "working"
  | "waiting_permission"
  | "waiting_question"
  | "needs_attention"
  | "idle"
  | "suspended"
  | "ended"
  | "failed"
>;
export type Model = Open<"opus" | "sonnet" | "fable">;
export type PermissionMode = Open<"manual" | "acceptEdits" | "plan">;

export type SessionSummary = {
  id: string;
  project_id: string;
  project_name: string;
  status: SessionStatus;
  started_at: number;
  last_activity_at: number;
  last_message: string | null;
  claude_session_id: string | null;
  model?: Model | null;
  permission_mode?: PermissionMode | null;
  agent?: string; // D7, absent from core output today
  host?: string; // D8, only if it survives
};

export type InboxKind = Open<"permission" | "question" | "finished" | "error">;
export type QuestionOption = { label: string; description: string | null };
export type Question = {
  question: string;
  header: string | null;
  multi_select: boolean;
  options: QuestionOption[];
};
export type InboxItem = {
  id: string;
  session_id: string;
  kind: InboxKind;
  created_at: number;
  resolved: boolean;
  expired: boolean;
  summary: string;
  tool_name: string | null;
  tool_input_preview: string | null;
  questions: Question[] | null;
  result_text: string | null;
  host?: string;
};

export type TaskStatus = Open<
  | "queued"
  | "running"
  | "needs_input"
  | "done"
  | "landing"
  | "landed"
  | "discarded"
  | "cleanup_pending"
  | "error"
>;
export type TaskSummary = {
  id: string;
  project_id: string;
  project_name: string;
  status: TaskStatus;
  title: string;
  created_at: number;
  session_id: string | null;
  branch: string | null;
  base_branch: string;
  note: string | null;
  last_turn_error: boolean;
  session_over: boolean;
  agent?: string;
  host?: string;
};

export type StorageStatus = { ok: boolean; reason?: string; found_version?: number };

export type Usage = { input: number; output: number; cache_read: number; cache_creation: number };
export type TranscriptEntry =
  | { kind: "user_prompt"; text: string; truncated: boolean }
  | { kind: "assistant_text"; text: string; truncated: boolean }
  | { kind: "tool_use"; id: string; name: string; input_preview: string; truncated: boolean }
  | { kind: "tool_result"; tool_use_id: string; is_error: boolean; content: string; truncated: boolean }
  | {
      kind: "turn_result";
      is_error: boolean;
      subtype: string | null;
      total_cost_usd: number | null;
      duration_ms: number | null;
      num_turns: number | null;
      usage: Usage | null;
    }
  | { kind: "truncated" }
  // A kind this build does not know; "unknown" is reserved and the host never emits it.
  | { kind: "unknown"; raw_kind: string; text: string | null };
export type TranscriptRecord = TranscriptEntry & { seq: number; at: number };
export type TranscriptBatch = { session_id: string; entries: TranscriptRecord[] };

// ---- Call arguments: the strict serde structs of the host ----

export type Answer =
  | { kind: "permission"; item_id: string; decision: "allow" | "deny"; message?: string }
  | { kind: "question"; item_id: string; answers: Record<string, string> }
  | { kind: "reply"; session_id: string; text: string };

export type StartArgs = {
  project_id: string;
  prompt: string;
  model?: "opus" | "sonnet" | "fable";
  permission_mode?: "manual" | "acceptEdits" | "plan";
};

export type NoArgs = Record<string, never>;

export type CallArgs = {
  list_projects: NoArgs;
  list_sessions: NoArgs;
  list_inbox: NoArgs;
  get_transcript: { session_id: string };
  list_tasks: NoArgs;
  get_task_diff: { task_id: string };
  storage_status: NoArgs;
  start_session: StartArgs;
  stop_session: { session_id: string };
  answer: Answer;
  create_task: StartArgs;
  cancel_task: { task_id: string };
  land_task: { task_id: string };
  discard_task: { task_id: string };
  retry_cleanup: { task_id: string };
};

// ---- Handshake: plaintext, before Noise, bound into its prologue ----

export type HandshakeRequest = { proto: "orchestra"; v: 1 };
export type HandshakeAccept = { proto: "orchestra"; v: 1; supported: number[] };

// ---- Client to host ----

export type Hello = { v: 1; type: "hello"; client_name: string; app_version: string };
export type Ping = { v: 1; type: "ping" };
export type Call = {
  [M in Method]: { v: 1; type: "call"; id: string; method: M; args: CallArgs[M] };
}[Method];
export type ClientMessage = Hello | Call | Ping;

// ---- Host to client ----

export type Welcome = {
  v: 1;
  type: "welcome";
  host_id: string;
  host_name: string;
  scopes: Open<Scope>[];
  features: string[];
};
export type Snapshot = {
  v: 1;
  type: "snapshot";
  seq: number;
  projects: Project[];
  sessions: SessionSummary[];
  inbox: InboxItem[];
  tasks: TaskSummary[];
  storage: StorageStatus;
};
export type HostEvent =
  | { v: 1; type: "event"; seq: number; name: "session-updated"; payload: SessionSummary }
  // inbox-updated carries the whole inbox of one session: every item of that session, never another
  // session's. A client replaces that session's items and keeps the rest. An empty payload names
  // no session and changes nothing. See protocol/README.md, What A2 decided.
  | { v: 1; type: "event"; seq: number; name: "inbox-updated"; payload: InboxItem[] }
  | { v: 1; type: "event"; seq: number; name: "transcript-appended"; payload: TranscriptBatch }
  | { v: 1; type: "event"; seq: number; name: "task-updated"; payload: TaskSummary };
// An event name this build does not know (including the reserved `notice` until H2). It
// encodes back as an ordinary `event`.
export type UnknownEvent = { v: 1; type: "unknown_event"; seq: number; name: string; payload: unknown };
export type ErrorBody = { code: Open<string>; message: string };
export type ResultOk = { v: 1; type: "result"; id: string; ok: unknown };
export type ResultError = { v: 1; type: "result"; id: string; error: ErrorBody };
export type ErrorMessage = {
  v: 1;
  type: "error";
  code: Open<string>;
  message: string;
  supported?: number[];
};
export type Pong = { v: 1; type: "pong" };
export type HostMessage =
  | Welcome
  | Snapshot
  | HostEvent
  | UnknownEvent
  | ResultOk
  | ResultError
  | ErrorMessage
  | Pong;

// What a client reads back to its plaintext handshake frame.
export type HandshakeReply = HandshakeAccept | ErrorMessage;
