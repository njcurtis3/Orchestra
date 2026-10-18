import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  DIFF_FILE_STATUSES,
  INBOX_KINDS,
  INTERFACE_KINDS,
  LINK_OSES,
  MODELS,
  PERMISSION_MODES,
  SCOPES,
  SESSION_STATUSES,
  TASK_STATUSES,
  type Answer,
  type CommitView,
  type Device,
  type DiffFile,
  type InboxItem,
  type LinkInterface,
  type LinkStatus,
  type Model,
  type PairingView,
  type PendingPairing,
  type PermissionMode,
  type Project,
  type Question,
  type Scope,
  type SessionSummary,
  type TaskDiff,
  type TaskSummary,
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

// Only the listed fields are read, so an extra field (a path-like one, say) never reaches
// the app. An unknown status throws and the caller drops the task.
export function decodeTask(v: unknown): TaskSummary {
  const o = obj(v, "task");
  return {
    id: str(o, "id"),
    project_id: str(o, "project_id"),
    project_name: str(o, "project_name"),
    status: oneOf(o, "status", TASK_STATUSES),
    title: str(o, "title"),
    created_at: num(o, "created_at"),
    session_id: strOrNull(o, "session_id"),
    branch: strOrNull(o, "branch"),
    base_branch: str(o, "base_branch"),
    note: strOrNull(o, "note"),
    last_turn_error: bool(o, "last_turn_error"),
    session_over: bool(o, "session_over"),
  };
}

function decodeDiffFile(v: unknown): DiffFile {
  const o = obj(v, "diff file");
  return {
    path: str(o, "path"),
    status: oneOf(o, "status", DIFF_FILE_STATUSES),
    added: num(o, "added"),
    removed: num(o, "removed"),
    binary: bool(o, "binary"),
  };
}

function decodeCommit(v: unknown): CommitView {
  const o = obj(v, "commit");
  return { id: str(o, "id"), subject: str(o, "subject"), body: str(o, "body") };
}

export function decodeTaskDiff(v: unknown): TaskDiff {
  const o = obj(v, "task diff");
  return {
    files: arr(o.files, "files").map(decodeDiffFile),
    files_truncated: bool(o, "files_truncated"),
    patch: str(o, "patch"),
    truncated: bool(o, "truncated"),
    commits: arr(o.commits, "commits").map(decodeCommit),
    base_branch: str(o, "base_branch"),
  };
}

// One bad task is dropped, not the whole list. Each drop is reported through onDrop.
export function decodeTasks(v: unknown, onDrop: (e: unknown) => void = () => {}): TaskSummary[] {
  return arr(v, "tasks").flatMap((x) => {
    try {
      return [decodeTask(x)];
    } catch (e) {
      onDrop(e);
      return [];
    }
  });
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

export async function createTask(
  projectId: string,
  prompt: string,
  model: Model | null = null,
  permissionMode: PermissionMode | null = null,
): Promise<TaskSummary> {
  const args = {
    project_id: projectId,
    prompt,
    ...(model && { model }),
    ...(permissionMode && { permission_mode: permissionMode }),
  };
  return decodeTask(await invoke<unknown>("create_task", { args }));
}
export async function listTasks(onDrop?: (e: unknown) => void): Promise<TaskSummary[]> {
  return decodeTasks(await invoke<unknown>("list_tasks"), onDrop);
}
export async function cancelTask(taskId: string): Promise<void> {
  await invoke<void>("cancel_task", { args: { task_id: taskId } });
}
export async function getTaskDiff(taskId: string): Promise<TaskDiff> {
  return decodeTaskDiff(await invoke<unknown>("get_task_diff", { args: { task_id: taskId } }));
}
export async function landTask(taskId: string): Promise<TaskSummary> {
  return decodeTask(await invoke<unknown>("land_task", { args: { task_id: taskId } }));
}
export async function discardTask(taskId: string): Promise<TaskSummary> {
  return decodeTask(await invoke<unknown>("discard_task", { args: { task_id: taskId } }));
}
export async function retryCleanup(taskId: string): Promise<TaskSummary> {
  return decodeTask(await invoke<unknown>("retry_cleanup", { args: { task_id: taskId } }));
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
export function onTaskUpdated(
  cb: (t: TaskSummary) => void,
  onError: (e: unknown) => void,
): Promise<UnlistenFn> {
  return listen<unknown>("task-updated", (e) => {
    try {
      cb(decodeTask(e.payload));
    } catch (err) {
      onError(err);
    }
  });
}

// ---- the phone link ----

const strs = (o: Obj, k: string): string[] =>
  arr(o[k], k).map((x) => {
    if (typeof x !== "string") throw new Error(`bad field ${k}`);
    return x;
  });

export function decodeScopes(v: unknown): Scope[] {
  return arr(v, "scopes").map((x) => {
    if (typeof x !== "string" || !(SCOPES as readonly string[]).includes(x)) throw new Error("unknown scope");
    return x as Scope;
  });
}

export function decodeLinkStatus(v: unknown): LinkStatus {
  const o = obj(v, "link status");
  return {
    available: bool(o, "available"),
    os: oneOf(o, "os", LINK_OSES),
    enabled: bool(o, "enabled"),
    running: bool(o, "running"),
    addrs: strs(o, "addrs"),
    port: num(o, "port"),
    binds: strs(o, "binds"),
    missing: strs(o, "missing"),
    error: strOrNull(o, "error"),
    config_readonly: strOrNull(o, "config_readonly"),
    firewall_notice_seen: bool(o, "firewall_notice_seen"),
  };
}

export function decodeLinkInterface(v: unknown): LinkInterface {
  const o = obj(v, "link interface");
  return { ip: str(o, "ip"), kind: oneOf(o, "kind", INTERFACE_KINDS), name: str(o, "name") };
}

// The pairing secret is in the QR and nowhere else: a payload that carries a secret field is
// refused, so it can never be put on screen as text by a later change on either side.
export function decodePairingView(v: unknown): PairingView {
  const o = obj(v, "pairing");
  for (const k of Object.keys(o)) {
    if (/secret|psk|private/i.test(k)) throw new Error("bad pairing");
  }
  const qr = str(o, "qr_svg");
  if (!/^(<\?xml[^>]*\?>)?<svg[\s>]/.test(qr)) throw new Error("bad field qr_svg");
  return { qr_svg: qr, host_name: str(o, "host_name"), addrs: strs(o, "addrs"), expires_at: num(o, "expires_at") };
}

export function decodePendingPairing(v: unknown): PendingPairing {
  const o = obj(v, "pairing request");
  return {
    id: str(o, "id"),
    client_name: str(o, "client_name"),
    fingerprint: str(o, "fingerprint"),
    expires_at: num(o, "expires_at"),
  };
}

export function decodeDevice(v: unknown): Device {
  const o = obj(v, "device");
  return {
    id: str(o, "id"),
    name: str(o, "name"),
    fingerprint: str(o, "fingerprint"),
    scopes: decodeScopes(o.scopes),
    paired_at: num(o, "paired_at"),
  };
}

export const decodeDevices = (v: unknown): Device[] => arr(v, "devices").map(decodeDevice);
export const decodeLinkInterfaces = (v: unknown): LinkInterface[] => arr(v, "interfaces").map(decodeLinkInterface);
export const decodePendingPairings = (v: unknown): PendingPairing[] => arr(v, "requests").map(decodePendingPairing);

export type LinkConfigChange = { enabled: boolean; binds: string[]; port: number; firewall_notice_seen: boolean };

export async function linkStatus(): Promise<LinkStatus> {
  return decodeLinkStatus(await invoke<unknown>("link_status"));
}
export async function linkInterfaces(): Promise<LinkInterface[]> {
  return decodeLinkInterfaces(await invoke<unknown>("link_interfaces"));
}
export async function linkSetConfig(change: LinkConfigChange): Promise<LinkStatus> {
  return decodeLinkStatus(await invoke<unknown>("link_set_config", { args: change }));
}
export async function linkOpenPairing(): Promise<PairingView> {
  return decodePairingView(await invoke<unknown>("link_open_pairing"));
}
export async function linkClosePairing(): Promise<void> {
  await invoke<void>("link_close_pairing");
}
export async function linkPending(): Promise<PendingPairing[]> {
  return decodePendingPairings(await invoke<unknown>("link_pending"));
}
export async function linkConfirmPairing(id: string, scopes: Scope[]): Promise<Device> {
  return decodeDevice(await invoke<unknown>("link_confirm_pairing", { args: { id, scopes } }));
}
export async function linkRejectPairing(id: string): Promise<void> {
  await invoke<void>("link_reject_pairing", { args: { id } });
}
export async function linkListDevices(): Promise<Device[]> {
  return decodeDevices(await invoke<unknown>("link_list_devices"));
}
export async function linkSetScopes(clientId: string, scopes: Scope[]): Promise<void> {
  await invoke<void>("link_set_scopes", { args: { client_id: clientId, scopes } });
}
export async function linkRevoke(clientId: string): Promise<void> {
  await invoke<void>("link_revoke", { args: { client_id: clientId } });
}

export function onLinkPairingRequest(
  cb: (p: PendingPairing) => void,
  onError: (e: unknown) => void,
): Promise<UnlistenFn> {
  return listen<unknown>("link-pairing-request", (e) => {
    try {
      cb(decodePendingPairing(e.payload));
    } catch (err) {
      onError(err);
    }
  });
}
export function onLinkStatusChanged(
  cb: (s: LinkStatus) => void,
  onError: (e: unknown) => void,
): Promise<UnlistenFn> {
  return listen<unknown>("link-status-changed", (e) => {
    try {
      cb(decodeLinkStatus(e.payload));
    } catch (err) {
      onError(err);
    }
  });
}
export function onLinkDevicesChanged(
  cb: (d: Device[]) => void,
  onError: (e: unknown) => void,
): Promise<UnlistenFn> {
  return listen<unknown>("link-devices-changed", (e) => {
    try {
      cb(decodeDevices(e.payload));
    } catch (err) {
      onError(err);
    }
  });
}
