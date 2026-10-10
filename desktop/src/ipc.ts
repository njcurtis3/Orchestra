import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  INBOX_KINDS,
  MODELS,
  PERMISSION_MODES,
  SESSION_STATUSES,
  type Answer,
  type InboxItem,
  type Model,
  type PermissionMode,
  type Project,
  type Question,
  type SessionSummary,
  type TranscriptBatch,
  type TranscriptPage,
  type TranscriptRecord,
  type Usage,
} from "./types";

type Obj = Record<string, unknown>;

function obj(v: unknown, what: string): Obj {
  if (typeof v !== "object" || v === null || Array.isArray(v)) {
    throw new Error(`bad ${what}`);
  }
  return v as Obj;
}
function str(o: Obj, k: string): string {
  const v = o[k];
  if (typeof v !== "string") throw new Error(`bad field ${k}`);
  return v;
}
function strOrNull(o: Obj, k: string): string | null {
  const v = o[k];
  if (v === null || v === undefined) return null;
  if (typeof v !== "string") throw new Error(`bad field ${k}`);
  return v;
}
function num(o: Obj, k: string): number {
  const v = o[k];
  if (typeof v !== "number" || !Number.isFinite(v)) throw new Error(`bad field ${k}`);
  return v;
}
function bool(o: Obj, k: string): boolean {
  const v = o[k];
  if (typeof v !== "boolean") throw new Error(`bad field ${k}`);
  return v;
}
function oneOf<T extends string>(o: Obj, k: string, allowed: readonly T[]): T {
  const v = o[k];
  if (typeof v !== "string" || !(allowed as readonly string[]).includes(v)) {
    throw new Error(`unknown ${k}`);
  }
  return v as T;
}
function oneOfOrNull<T extends string>(o: Obj, k: string, allowed: readonly T[]): T | null {
  return o[k] === null || o[k] === undefined ? null : oneOf(o, k, allowed);
}
function arr(v: unknown, what: string): unknown[] {
  if (!Array.isArray(v)) throw new Error(`bad ${what}`);
  return v;
}

export function decodeProject(v: unknown): Project {
  const o = obj(v, "project");
  return { id: str(o, "id"), name: str(o, "name") };
}

export function decodeSession(v: unknown): SessionSummary {
  const o = obj(v, "session");
  return {
    id: str(o, "id"),
    project_id: str(o, "project_id"),
    project_name: str(o, "project_name"),
    status: oneOf(o, "status", SESSION_STATUSES),
    started_at: num(o, "started_at"),
    last_activity_at: num(o, "last_activity_at"),
    last_message: strOrNull(o, "last_message"),
    claude_session_id: strOrNull(o, "claude_session_id"),
    model: oneOfOrNull(o, "model", MODELS),
    permission_mode: oneOfOrNull(o, "permission_mode", PERMISSION_MODES),
  };
}

function decodeQuestion(v: unknown): Question {
  const o = obj(v, "question");
  return {
    question: str(o, "question"),
    header: strOrNull(o, "header"),
    multi_select: bool(o, "multi_select"),
    options: arr(o.options, "options").map((x) => {
      const p = obj(x, "option");
      return { label: str(p, "label"), description: strOrNull(p, "description") };
    }),
  };
}

export function decodeInboxItem(v: unknown): InboxItem {
  const o = obj(v, "inbox item");
  const qs = o.questions;
  return {
    id: str(o, "id"),
    session_id: str(o, "session_id"),
    kind: oneOf(o, "kind", INBOX_KINDS),
    created_at: num(o, "created_at"),
    resolved: bool(o, "resolved"),
    expired: bool(o, "expired"),
    summary: str(o, "summary"),
    tool_name: strOrNull(o, "tool_name"),
    tool_input_preview: strOrNull(o, "tool_input_preview"),
    questions: qs === null || qs === undefined ? null : arr(qs, "questions").map(decodeQuestion),
    result_text: strOrNull(o, "result_text"),
  };
}

function numOrNull(o: Obj, k: string): number | null {
  const v = o[k];
  if (v === null || v === undefined) return null;
  return num(o, k);
}

function decodeUsage(v: unknown): Usage {
  const o = obj(v, "usage");
  return { input: num(o, "input"), output: num(o, "output"), cache_read: num(o, "cache_read"), cache_creation: num(o, "cache_creation") };
}

export function decodeTranscriptRecord(v: unknown): TranscriptRecord {
  const o = obj(v, "transcript entry");
  const base = { seq: num(o, "seq"), at: num(o, "at") };
  switch (o.kind) {
    case "user_prompt":
    case "assistant_text":
      return { ...base, kind: o.kind, text: str(o, "text"), truncated: bool(o, "truncated") };
    case "tool_use":
      return {
        ...base,
        kind: "tool_use",
        id: str(o, "id"),
        name: str(o, "name"),
        input_preview: str(o, "input_preview"),
        truncated: bool(o, "truncated"),
      };
    case "tool_result":
      return {
        ...base,
        kind: "tool_result",
        tool_use_id: str(o, "tool_use_id"),
        is_error: bool(o, "is_error"),
        content: str(o, "content"),
        truncated: bool(o, "truncated"),
      };
    case "turn_result":
      return {
        ...base,
        kind: "turn_result",
        is_error: bool(o, "is_error"),
        subtype: strOrNull(o, "subtype"),
        total_cost_usd: numOrNull(o, "total_cost_usd"),
        duration_ms: numOrNull(o, "duration_ms"),
        num_turns: numOrNull(o, "num_turns"),
        usage: o.usage === null || o.usage === undefined ? null : decodeUsage(o.usage),
      };
    case "truncated":
      return { ...base, kind: "truncated" };
    default:
      throw new Error("unknown kind");
  }
}

// One bad entry is dropped, not the whole list: the rest of the transcript is still good.
export function decodeTranscriptEntries(v: unknown): TranscriptRecord[] {
  return arr(v, "transcript").flatMap((x) => {
    try {
      return [decodeTranscriptRecord(x)];
    } catch {
      return [];
    }
  });
}

export function decodeTranscript(v: unknown): TranscriptPage {
  const o = obj(v, "transcript");
  return { entries: decodeTranscriptEntries(o.entries), next_seq: num(o, "next_seq") };
}

export function decodeTranscriptBatch(v: unknown): TranscriptBatch {
  const o = obj(v, "transcript batch");
  return { session_id: str(o, "session_id"), entries: decodeTranscriptEntries(o.entries) };
}

// Whether history was loaded and is being saved. Carries no path.
export type StorageStatus =
  | { ok: true }
  | { ok: false; reason: "unknown_schema_version" | "unreadable"; found_version: number | null };

export function decodeStorageStatus(v: unknown): StorageStatus {
  const o = obj(v, "storage status");
  if (bool(o, "ok")) return { ok: true };
  const reason = oneOf(o, "reason", ["unknown_schema_version", "unreadable"] as const);
  const found = o.found_version;
  return { ok: false, reason, found_version: found === null || found === undefined ? null : num(o, "found_version") };
}

export const decodeSessions = (v: unknown): SessionSummary[] =>
  arr(v, "sessions").map(decodeSession);
export const decodeInbox = (v: unknown): InboxItem[] => arr(v, "inbox").map(decodeInboxItem);
export const decodeProjects = (v: unknown): Project[] => arr(v, "projects").map(decodeProject);

export async function pickProject(): Promise<Project | null> {
  const r = await invoke<unknown>("pick_project");
  return r === null || r === undefined ? null : decodeProject(r);
}
export async function listProjects(): Promise<Project[]> {
  return decodeProjects(await invoke<unknown>("list_projects"));
}
// A null setting is left out of the call, so Rust passes no flag for it.
export async function startSession(
  projectId: string,
  prompt: string,
  model: Model | null = null,
  permissionMode: PermissionMode | null = null,
): Promise<SessionSummary> {
  const args = {
    project_id: projectId,
    prompt,
    ...(model && { model }),
    ...(permissionMode && { permission_mode: permissionMode }),
  };
  return decodeSession(await invoke<unknown>("start_session", { args }));
}
export async function storageStatus(): Promise<StorageStatus> {
  return decodeStorageStatus(await invoke<unknown>("storage_status"));
}
export async function listSessions(): Promise<SessionSummary[]> {
  return decodeSessions(await invoke<unknown>("list_sessions"));
}
export async function listInbox(): Promise<InboxItem[]> {
  return decodeInbox(await invoke<unknown>("list_inbox"));
}
export async function answer(a: Answer): Promise<void> {
  await invoke<void>("answer", { args: a });
}
export async function getTranscript(sessionId: string): Promise<TranscriptPage> {
  return decodeTranscript(await invoke<unknown>("get_transcript", { args: { session_id: sessionId } }));
}
export async function stopSession(sessionId: string): Promise<void> {
  await invoke<void>("stop_session", { args: { session_id: sessionId } });
}

// A payload that fails to decode is dropped (reported via onError), never applied.
export function onSessionUpdated(
  cb: (s: SessionSummary) => void,
  onError: (e: unknown) => void,
): Promise<UnlistenFn> {
  return listen<unknown>("session-updated", (e) => {
    try {
      cb(decodeSession(e.payload));
    } catch (err) {
      onError(err);
    }
  });
}
export function onInboxUpdated(
  cb: (items: InboxItem[]) => void,
  onError: (e: unknown) => void,
): Promise<UnlistenFn> {
  return listen<unknown>("inbox-updated", (e) => {
    try {
      cb(decodeInbox(e.payload));
    } catch (err) {
      onError(err);
    }
  });
}
export function onTranscriptAppended(
  cb: (b: TranscriptBatch) => void,
  onError: (e: unknown) => void,
): Promise<UnlistenFn> {
  return listen<unknown>("transcript-appended", (e) => {
    try {
      cb(decodeTranscriptBatch(e.payload));
    } catch (err) {
      onError(err);
    }
  });
}
