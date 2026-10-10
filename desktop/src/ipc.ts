import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  INBOX_KINDS,
  SESSION_STATUSES,
  type Answer,
  type InboxItem,
  type Project,
  type Question,
  type SessionSummary,
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
export async function startSession(projectId: string, prompt: string): Promise<SessionSummary> {
  return decodeSession(
    await invoke<unknown>("start_session", { args: { project_id: projectId, prompt } }),
  );
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
