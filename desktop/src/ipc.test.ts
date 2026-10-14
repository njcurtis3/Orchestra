import { describe, expect, it } from "vitest";
import {
  decodeInboxItem,
  decodeSession,
  decodeStorageStatus,
  decodeTranscript,
  decodeTranscriptBatch,
  decodeTranscriptRecord,
} from "./ipc";

const wire = (status: unknown) => ({
  id: "s1",
  project_id: "p1",
  project_name: "proj",
  status,
  started_at: 1,
  last_activity_at: 2,
  last_message: null,
  claude_session_id: null,
});

describe("decodeSession settings", () => {
  it("decodes a listed model and mode, and null or absent as null", () => {
    const s = decodeSession({ ...wire("working"), model: "fable", permission_mode: "acceptEdits" });
    expect([s.model, s.permission_mode]).toEqual(["fable", "acceptEdits"]);
    expect(decodeSession({ ...wire("working"), model: null }).model).toBeNull();
    expect(decodeSession(wire("working")).permission_mode).toBeNull();
  });

  it("rejects a model or mode outside the lists", () => {
    expect(() => decodeSession({ ...wire("working"), model: "haiku" })).toThrow("unknown model");
    expect(() => decodeSession({ ...wire("working"), permission_mode: "bypassPermissions" })).toThrow(
      "unknown permission_mode",
    );
  });
});

describe("decodeSession status", () => {
  it("accepts needs_attention", () => {
    expect(decodeSession(wire("needs_attention")).status).toBe("needs_attention");
  });

  it("rejects an unknown status", () => {
    expect(() => decodeSession(wire("on_fire"))).toThrow("unknown status");
  });
});

const itemWire = (over: Record<string, unknown> = {}) => ({
  id: "s1-i1",
  session_id: "s1",
  kind: "finished",
  created_at: 1,
  resolved: false,
  expired: false,
  summary: "done",
  tool_name: null,
  tool_input_preview: null,
  questions: null,
  result_text: null,
  ...over,
});

describe("decodeSession suspended", () => {
  it("accepts suspended", () => {
    expect(decodeSession(wire("suspended")).status).toBe("suspended");
  });
});

describe("decodeInboxItem", () => {
  it("decodes expired and result_text", () => {
    const i = decodeInboxItem(itemWire({ resolved: true, expired: true, result_text: "full text" }));
    expect([i.expired, i.result_text]).toEqual([true, "full text"]);
  });

  it("treats a missing result_text as null", () => {
    const { result_text: _drop, ...rest } = itemWire();
    expect(decodeInboxItem(rest).result_text).toBeNull();
  });

  it("rejects an item missing expired", () => {
    const { expired: _drop, ...rest } = itemWire();
    expect(() => decodeInboxItem(rest)).toThrow("bad field expired");
  });

  it("rejects a mistyped expired or result_text", () => {
    expect(() => decodeInboxItem(itemWire({ expired: "yes" }))).toThrow("bad field expired");
    expect(() => decodeInboxItem(itemWire({ result_text: 3 }))).toThrow("bad field result_text");
  });
});

describe("decodeStorageStatus", () => {
  it("decodes ok and the two failure reasons", () => {
    expect(decodeStorageStatus({ ok: true })).toEqual({ ok: true });
    expect(decodeStorageStatus({ ok: false, reason: "unknown_schema_version", found_version: 2 })).toEqual({
      ok: false,
      reason: "unknown_schema_version",
      found_version: 2,
    });
    expect(decodeStorageStatus({ ok: false, reason: "unreadable" })).toEqual({
      ok: false,
      reason: "unreadable",
      found_version: null,
    });
  });

  it("rejects an unknown reason", () => {
    expect(() => decodeStorageStatus({ ok: false, reason: "disk_on_fire" })).toThrow("unknown reason");
  });
});

describe("decodeTranscript", () => {
  const rec = (over: Record<string, unknown>) => ({ seq: 1, at: 5, ...over });
  const good = [
    rec({ kind: "user_prompt", text: "hi", truncated: false }),
    rec({ seq: 2, kind: "tool_use", id: "t", name: "Bash", input_preview: "{}", truncated: false }),
    rec({ seq: 3, kind: "tool_result", tool_use_id: "t", is_error: false, content: "ok", truncated: true }),
    rec({
      seq: 4,
      kind: "turn_result",
      is_error: false,
      subtype: "success",
      total_cost_usd: 0.5,
      duration_ms: null,
      num_turns: 2,
      usage: { input: 1, output: 2, cache_read: 3, cache_creation: 4 },
    }),
    rec({ seq: 5, kind: "truncated" }),
  ];

  it("decodes every entry kind", () => {
    const page = decodeTranscript({ entries: good, next_seq: 6 });
    expect(page.entries.map((e) => e.kind)).toEqual(["user_prompt", "tool_use", "tool_result", "turn_result", "truncated"]);
    expect(page.next_seq).toBe(6);
  });

  it("drops malformed entries and keeps the rest", () => {
    const bad = [
      rec({ seq: 7, kind: "alarm" }),
      rec({ seq: 8, kind: "assistant_text", text: 3, truncated: false }),
      rec({ seq: 9, kind: "tool_result", tool_use_id: "t", is_error: "no", content: "", truncated: false }),
      { kind: "user_prompt", text: "no seq", truncated: false },
      "junk",
      null,
    ];
    const page = decodeTranscript({ entries: [good[0], ...bad, good[4]], next_seq: 10 });
    expect(page.entries.map((e) => e.seq)).toEqual([1, 5]);
  });

  it("rejects a page or batch whose envelope is wrong", () => {
    expect(() => decodeTranscript({ entries: good })).toThrow("bad field next_seq");
    expect(() => decodeTranscript({ entries: "x", next_seq: 1 })).toThrow();
    expect(() => decodeTranscriptBatch({ entries: [] })).toThrow("bad field session_id");
    expect(decodeTranscriptBatch({ session_id: "s1", entries: [good[0], { kind: "x" }] }).entries).toHaveLength(1);
  });

  it("throws on a single bad record", () => {
    expect(() => decodeTranscriptRecord(rec({ kind: "tool_use", id: "t" }))).toThrow();
  });
});
