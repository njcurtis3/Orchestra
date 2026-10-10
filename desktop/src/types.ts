// Mirrors the Rust IPC contract field for field. Wire names are snake_case.

export type Project = { id: string; name: string };

export const SESSION_STATUSES = [
  "starting",
  "working",
  "waiting_permission",
  "waiting_question",
  "needs_attention",
  "idle",
  "ended",
  "failed",
] as const;
export type SessionStatus = (typeof SESSION_STATUSES)[number];

export type SessionSummary = {
  id: string;
  project_id: string;
  project_name: string;
  status: SessionStatus;
  started_at: number; // ms epoch
  last_activity_at: number; // ms epoch
  last_message: string | null; // truncated to 500 chars
  claude_session_id: string | null;
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
  summary: string; // truncated
  tool_name: string | null;
  tool_input_preview: string | null; // JSON, truncated to 2000 chars
  questions: Question[] | null;
};

export type Answer =
  | { kind: "permission"; item_id: string; decision: "allow" | "deny"; message?: string }
  | { kind: "question"; item_id: string; answers: Record<string, string> }
  | { kind: "reply"; session_id: string; text: string };

export const MAX_TEXT_BYTES = 32768;
