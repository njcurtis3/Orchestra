// Mirrors the Rust IPC contract field for field. Wire names are snake_case.

export type Project = { id: string; name: string };

export const SESSION_STATUSES = [
  "starting",
  "working",
  "waiting_permission",
  "waiting_question",
  "needs_attention",
  "idle",
  "suspended",
  "ended",
  "failed",
] as const;
export type SessionStatus = (typeof SESSION_STATUSES)[number];

// Per-session claude settings. Rust holds the same closed lists; `null` means no flag.
export const MODELS = ["opus", "sonnet", "fable"] as const;
export type Model = (typeof MODELS)[number];
export const PERMISSION_MODES = ["manual", "acceptEdits", "plan"] as const;
export type PermissionMode = (typeof PERMISSION_MODES)[number];

export type SessionSummary = {
  id: string;
  project_id: string;
  project_name: string;
  status: SessionStatus;
  started_at: number; // ms epoch
  last_activity_at: number; // ms epoch
  last_message: string | null; // truncated to 500 chars
  claude_session_id: string | null;
  // Optional in the type only so older fixtures still compile; the decoder always sets them.
  model?: Model | null;
  permission_mode?: PermissionMode | null;
};

export const INBOX_KINDS = ["permission", "question", "finished", "error"] as const;
export type InboxKind = (typeof INBOX_KINDS)[number];

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
  expired: boolean; // open when Orchestra closed; implies resolved, never shown as Resolved
  summary: string; // truncated
  tool_name: string | null;
  tool_input_preview: string | null; // JSON, truncated to 2000 chars
  questions: Question[] | null;
  result_text: string | null; // full result text on finished/error items, capped at 256 KiB
};

// The readable record of a session, served by get_transcript and pushed as transcript-appended.
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
      total_cost_usd: number | null; // the session total as claude reports it, never a sum
      duration_ms: number | null;
      num_turns: number | null;
      usage: Usage | null; // this turn
    }
  | { kind: "truncated" };

export type TranscriptRecord = TranscriptEntry & { seq: number; at: number };
export type TranscriptPage = { entries: TranscriptRecord[]; next_seq: number };
export type TranscriptBatch = { session_id: string; entries: TranscriptRecord[] };

export type Answer =
  | { kind: "permission"; item_id: string; decision: "allow" | "deny"; message?: string }
  | { kind: "question"; item_id: string; answers: Record<string, string> }
  | { kind: "reply"; session_id: string; text: string };

export const MAX_TEXT_BYTES = 32768;
