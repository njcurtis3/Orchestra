// Protocol v1 codec. Host input (client to host) is strict: an unknown field is an error.
// Client input (host to client) is tolerant: unknown fields are dropped and unknown enum
// values are kept as the strings they arrived as. Both reject an unknown `v` loudly.
//
// Order of checks, identical in the Rust copy: size, JSON, object, `v`, `type`, fields.
import {
  ERROR_CODES,
  MAX_ID_BYTES,
  MAX_MESSAGE_BYTES,
  MAX_TEXT_BYTES,
  METHODS,
  PROTOCOL_NAME,
  PROTOCOL_VERSION,
  SUPPORTED_VERSIONS,
  type ErrorCode,
  type Method,
} from "./constants";
import type {
  Answer,
  Call,
  CallArgs,
  ClientMessage,
  ErrorMessage,
  HandshakeAccept,
  HandshakeReply,
  HandshakeRequest,
  HostMessage,
  InboxItem,
  Project,
  Question,
  SessionSummary,
  StartArgs,
  StorageStatus,
  TaskSummary,
  TranscriptBatch,
  TranscriptRecord,
} from "./types";

export class ProtocolError extends Error {
  readonly code: ErrorCode;
  constructor(code: ErrorCode, message: string) {
    super(message);
    this.name = "ProtocolError";
    this.code = code;
  }
}

type Obj = Record<string, unknown>;

function bad(message: string): never {
  throw new ProtocolError("bad_request", message);
}

export function utf8Length(s: string): number {
  let n = 0;
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    if (c < 0x80) n += 1;
    else if (c < 0x800) n += 2;
    else if (c >= 0xd800 && c < 0xdc00 && i + 1 < s.length) {
      n += 4; // a surrogate pair is one 4-byte code point
      i++;
    } else n += 3;
  }
  return n;
}

// ---- Envelope ----

function envelope(text: string): Obj {
  if (typeof text !== "string" || utf8Length(text) > MAX_MESSAGE_BYTES) bad("message too large");
  let v: unknown;
  try {
    v = JSON.parse(text);
  } catch {
    return bad("not JSON");
  }
  const o = obj(v, "message");
  if (!("v" in o)) bad("missing v");
  if (!Number.isInteger(o.v)) bad("bad v");
  if (!SUPPORTED_VERSIONS.includes(o.v as number)) {
    throw new ProtocolError("unsupported_version", `unsupported version ${String(o.v)}`);
  }
  return o;
}

function obj(v: unknown, what: string): Obj {
  if (typeof v !== "object" || v === null || Array.isArray(v)) return bad(`bad ${what}`);
  return v as Obj;
}

function exact(o: Obj, keys: string[], what: string): void {
  for (const k of Object.keys(o)) if (!keys.includes(k)) bad(`unknown field ${k} in ${what}`);
}

function typeOf(o: Obj): string {
  if (typeof o.type !== "string") return bad("missing type");
  return o.type;
}

function str(o: Obj, k: string): string {
  const v = o[k];
  if (typeof v !== "string") return bad(`bad field ${k}`);
  return v;
}

// Null and absent both read as "no value" on host input (serde Option does the same).
function optStr(o: Obj, k: string): string | undefined {
  const v = o[k];
  if (v === undefined || v === null) return undefined;
  if (typeof v !== "string") return bad(`bad field ${k}`);
  return v;
}

function oneOf<T extends string>(o: Obj, k: string, allowed: readonly T[]): T {
  const v = o[k];
  if (typeof v !== "string" || !(allowed as readonly string[]).includes(v)) return bad(`bad field ${k}`);
  return v as T;
}

function uint(o: Obj, k: string): number {
  const v = o[k];
  if (typeof v !== "number" || !Number.isSafeInteger(v) || v < 0) return bad(`bad field ${k}`);
  return v;
}

function num(o: Obj, k: string): number {
  const v = o[k];
  if (typeof v !== "number" || !Number.isFinite(v)) return bad(`bad field ${k}`);
  return v;
}

function bool(o: Obj, k: string): boolean {
  const v = o[k];
  if (typeof v !== "boolean") return bad(`bad field ${k}`);
  return v;
}

function arr<T>(o: Obj, k: string, each: (v: unknown) => T): T[] {
  const v = o[k];
  if (!Array.isArray(v)) return bad(`bad field ${k}`);
  return v.map(each);
}

function nullable<T>(o: Obj, k: string, read: (o: Obj, k: string) => T): T | null {
  return o[k] === null || o[k] === undefined ? null : read(o, k);
}

function strings(v: unknown): string {
  if (typeof v !== "string") return bad("bad string");
  return v;
}

function ints(o: Obj, k: string): number[] {
  return arr(o, k, (v) => {
    if (typeof v !== "number" || !Number.isSafeInteger(v)) return bad(`bad field ${k}`);
    return v;
  });
}

// ---- Client to host (strict) ----

function capStrings(v: unknown): void {
  if (typeof v === "string") {
    if (utf8Length(v) > MAX_TEXT_BYTES) bad("text too long");
  } else if (Array.isArray(v)) {
    v.forEach(capStrings);
  } else if (typeof v === "object" && v !== null) {
    for (const [k, x] of Object.entries(v)) {
      capStrings(k);
      capStrings(x);
    }
  }
}

function startArgs(a: Obj): StartArgs {
  exact(a, ["project_id", "prompt", "model", "permission_mode"], "args");
  const out: StartArgs = { project_id: str(a, "project_id"), prompt: str(a, "prompt") };
  if (a.model !== undefined && a.model !== null) out.model = oneOf(a, "model", ["opus", "sonnet", "fable"] as const);
  if (a.permission_mode !== undefined && a.permission_mode !== null) {
    out.permission_mode = oneOf(a, "permission_mode", ["manual", "acceptEdits", "plan"] as const);
  }
  return out;
}

function answerArgs(a: Obj): Answer {
  switch (a.kind) {
    case "permission": {
      exact(a, ["kind", "item_id", "decision", "message"], "args");
      const out: Answer = {
        kind: "permission",
        item_id: str(a, "item_id"),
        decision: oneOf(a, "decision", ["allow", "deny"]),
      };
      const message = optStr(a, "message");
      if (message !== undefined) out.message = message;
      return out;
    }
    case "question": {
      exact(a, ["kind", "item_id", "answers"], "args");
      const answers = obj(a.answers, "answers");
      const out: Record<string, string> = {};
      for (const k of Object.keys(answers)) out[k] = str(answers, k);
      return { kind: "question", item_id: str(a, "item_id"), answers: out };
    }
    case "reply":
      exact(a, ["kind", "session_id", "text"], "args");
      return { kind: "reply", session_id: str(a, "session_id"), text: str(a, "text") };
    default:
      return bad("bad answer kind");
  }
}

function callArgs(method: Method, raw: unknown): CallArgs[Method] {
  const a = obj(raw, "args");
  switch (method) {
    case "list_projects":
    case "list_sessions":
    case "list_inbox":
    case "list_tasks":
    case "storage_status":
      exact(a, [], "args");
      return {};
    case "get_transcript":
    case "stop_session":
      exact(a, ["session_id"], "args");
      return { session_id: str(a, "session_id") };
    case "get_task_diff":
    case "cancel_task":
    case "land_task":
    case "discard_task":
    case "retry_cleanup":
      exact(a, ["task_id"], "args");
      return { task_id: str(a, "task_id") };
    case "start_session":
    case "create_task":
      return startArgs(a);
    case "answer":
      return answerArgs(a);
  }
}

export function decodeClientMessage(text: string): ClientMessage {
  const o = envelope(text);
  let msg: ClientMessage;
  switch (typeOf(o)) {
    case "hello":
      exact(o, ["v", "type", "client_name", "app_version"], "hello");
      msg = { v: 1, type: "hello", client_name: str(o, "client_name"), app_version: str(o, "app_version") };
      break;
    case "ping":
      exact(o, ["v", "type"], "ping");
      msg = { v: 1, type: "ping" };
      break;
    case "call": {
      exact(o, ["v", "type", "id", "method", "args"], "call");
      const id = str(o, "id");
      if (id.length === 0 || utf8Length(id) > MAX_ID_BYTES) bad("bad call id");
      const method = oneOf(o, "method", METHODS);
      msg = { v: 1, type: "call", id, method, args: callArgs(method, o.args) } as Call;
      break;
    }
    default:
      throw new ProtocolError("unknown_type", `unknown type ${String(o.type)}`);
  }
  capStrings(msg);
  return msg;
}

function checked(json: string): string {
  if (utf8Length(json) > MAX_MESSAGE_BYTES) bad("message too large");
  return json;
}

export function encodeClientMessage(m: ClientMessage): string {
  return checked(JSON.stringify(m));
}

// ---- Host to client (tolerant) ----

function project(v: unknown): Project {
  const o = obj(v, "project");
  return { id: str(o, "id"), name: str(o, "name") };
}

// Keeps the optional D7/D8 fields only when the host sent them.
function withExtras<T extends Obj>(o: Obj, out: T, keys: string[]): T {
  for (const k of keys) {
    const x = optStr(o, k);
    if (x !== undefined) (out as Obj)[k] = x;
  }
  return out;
}

function session(v: unknown): SessionSummary {
  const o = obj(v, "session");
  const out: SessionSummary = {
    id: str(o, "id"),
    project_id: str(o, "project_id"),
    project_name: str(o, "project_name"),
    status: str(o, "status"),
    started_at: uint(o, "started_at"),
    last_activity_at: uint(o, "last_activity_at"),
    last_message: nullable(o, "last_message", str),
    claude_session_id: nullable(o, "claude_session_id", str),
  };
  if (o.model !== undefined) out.model = nullable(o, "model", str);
  if (o.permission_mode !== undefined) out.permission_mode = nullable(o, "permission_mode", str);
  return withExtras(o, out, ["agent", "host"]);
}

function question(v: unknown): Question {
  const o = obj(v, "question");
  return {
    question: str(o, "question"),
    header: nullable(o, "header", str),
    multi_select: bool(o, "multi_select"),
    options: arr(o, "options", (x) => {
      const p = obj(x, "option");
      return { label: str(p, "label"), description: nullable(p, "description", str) };
    }),
  };
}

function inboxItem(v: unknown): InboxItem {
  const o = obj(v, "inbox item");
  const out: InboxItem = {
    id: str(o, "id"),
    session_id: str(o, "session_id"),
    kind: str(o, "kind"),
    created_at: uint(o, "created_at"),
    resolved: bool(o, "resolved"),
    expired: bool(o, "expired"),
    summary: str(o, "summary"),
    tool_name: nullable(o, "tool_name", str),
    tool_input_preview: nullable(o, "tool_input_preview", str),
    questions: o.questions === null || o.questions === undefined ? null : arr(o, "questions", question),
    result_text: nullable(o, "result_text", str),
  };
  return withExtras(o, out, ["host"]);
}

function task(v: unknown): TaskSummary {
  const o = obj(v, "task");
  const out: TaskSummary = {
    id: str(o, "id"),
    project_id: str(o, "project_id"),
    project_name: str(o, "project_name"),
    status: str(o, "status"),
    title: str(o, "title"),
    created_at: uint(o, "created_at"),
    session_id: nullable(o, "session_id", str),
    branch: nullable(o, "branch", str),
    base_branch: str(o, "base_branch"),
    note: nullable(o, "note", str),
    last_turn_error: bool(o, "last_turn_error"),
    session_over: bool(o, "session_over"),
  };
  return withExtras(o, out, ["agent", "host"]);
}

function storage(v: unknown): StorageStatus {
  const o = obj(v, "storage");
  const out: StorageStatus = { ok: bool(o, "ok") };
  if (o.reason !== undefined) out.reason = str(o, "reason");
  if (o.found_version !== undefined) out.found_version = uint(o, "found_version");
  return out;
}

function record(v: unknown): TranscriptRecord {
  const o = obj(v, "entry");
  const at = { seq: uint(o, "seq"), at: uint(o, "at") };
  switch (o.kind) {
    case "user_prompt":
    case "assistant_text":
      return { kind: o.kind, text: str(o, "text"), truncated: bool(o, "truncated"), ...at };
    case "tool_use":
      return {
        kind: "tool_use",
        id: str(o, "id"),
        name: str(o, "name"),
        input_preview: str(o, "input_preview"),
        truncated: bool(o, "truncated"),
        ...at,
      };
    case "tool_result":
      return {
        kind: "tool_result",
        tool_use_id: str(o, "tool_use_id"),
        is_error: bool(o, "is_error"),
        content: str(o, "content"),
        truncated: bool(o, "truncated"),
        ...at,
      };
    case "turn_result":
      return {
        kind: "turn_result",
        is_error: bool(o, "is_error"),
        subtype: nullable(o, "subtype", str),
        total_cost_usd: nullable(o, "total_cost_usd", num),
        duration_ms: nullable(o, "duration_ms", uint),
        num_turns: nullable(o, "num_turns", uint),
        usage: nullable(o, "usage", (p, k) => {
          const u = obj(p[k], "usage");
          return {
            input: uint(u, "input"),
            output: uint(u, "output"),
            cache_read: uint(u, "cache_read"),
            cache_creation: uint(u, "cache_creation"),
          };
        }),
        ...at,
      };
    case "truncated":
      return { kind: "truncated", ...at };
    default:
      return bad("bad entry kind");
  }
}

function batch(v: unknown): TranscriptBatch {
  const o = obj(v, "batch");
  return { session_id: str(o, "session_id"), entries: arr(o, "entries", record) };
}

function list<T>(v: unknown, each: (x: unknown) => T): T[] {
  if (!Array.isArray(v)) return bad("bad payload");
  return v.map(each);
}

function errorMessage(o: Obj): ErrorMessage {
  const out: ErrorMessage = { v: 1, type: "error", code: str(o, "code"), message: str(o, "message") };
  if (o.supported !== undefined) out.supported = ints(o, "supported");
  return out;
}

export function decodeHostMessage(text: string): HostMessage {
  const o = envelope(text);
  switch (typeOf(o)) {
    case "welcome":
      return {
        v: 1,
        type: "welcome",
        host_id: str(o, "host_id"),
        host_name: str(o, "host_name"),
        scopes: arr(o, "scopes", strings),
        features: arr(o, "features", strings),
      };
    case "snapshot":
      return {
        v: 1,
        type: "snapshot",
        seq: uint(o, "seq"),
        projects: arr(o, "projects", project),
        sessions: arr(o, "sessions", session),
        inbox: arr(o, "inbox", inboxItem),
        tasks: arr(o, "tasks", task),
        storage: storage(o.storage),
      };
    case "event": {
      const seq = uint(o, "seq");
      const name = str(o, "name");
      switch (name) {
        case "session-updated":
          return { v: 1, type: "event", seq, name, payload: session(o.payload) };
        case "inbox-updated":
          return { v: 1, type: "event", seq, name, payload: list(o.payload, inboxItem) };
        case "transcript-appended":
          return { v: 1, type: "event", seq, name, payload: batch(o.payload) };
        case "task-updated":
          return { v: 1, type: "event", seq, name, payload: task(o.payload) };
        default:
          return { v: 1, type: "unknown_event", seq, name, payload: o.payload };
      }
    }
    case "result": {
      const id = str(o, "id");
      const hasOk = "ok" in o;
      const hasError = "error" in o;
      if (hasOk === hasError) return bad("result needs exactly one of ok and error");
      if (hasOk) return { v: 1, type: "result", id, ok: o.ok };
      const e = obj(o.error, "error");
      return { v: 1, type: "result", id, error: { code: str(e, "code"), message: str(e, "message") } };
    }
    case "error":
      return errorMessage(o);
    case "pong":
      return { v: 1, type: "pong" };
    default:
      throw new ProtocolError("unknown_type", `unknown type ${String(o.type)}`);
  }
}

export function encodeHostMessage(m: HostMessage): string {
  const wire = m.type === "unknown_event" ? { ...m, type: "event" } : m;
  return checked(JSON.stringify(wire));
}

// ---- Handshake: plaintext frames with no `type` ----

export function decodeHandshakeRequest(text: string): HandshakeRequest {
  const o = envelope(text);
  if (o.proto !== PROTOCOL_NAME) bad("bad proto");
  exact(o, ["proto", "v"], "handshake");
  return { proto: PROTOCOL_NAME, v: PROTOCOL_VERSION };
}

export function encodeHandshakeRequest(
  r: HandshakeRequest = { proto: PROTOCOL_NAME, v: PROTOCOL_VERSION },
): string {
  return checked(JSON.stringify(r));
}

// The host's answer: {proto, v, supported}, or an error message (unsupported_version).
export function decodeHandshakeReply(text: string): HandshakeReply {
  const o = envelope(text);
  if (o.type === "error") return errorMessage(o);
  if (o.proto !== PROTOCOL_NAME) bad("bad proto");
  return { proto: PROTOCOL_NAME, v: PROTOCOL_VERSION, supported: ints(o, "supported") };
}

export function encodeHandshakeReply(
  r: HandshakeAccept | ErrorMessage = {
    proto: PROTOCOL_NAME,
    v: PROTOCOL_VERSION,
    supported: [...SUPPORTED_VERSIONS],
  },
): string {
  return checked(JSON.stringify(r));
}

export function isErrorCode(code: string): code is ErrorCode {
  return (ERROR_CODES as readonly string[]).includes(code);
}
