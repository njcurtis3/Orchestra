import { describe, expect, it } from "vitest";
import { decodeInboxItem, decodeSession } from "./ipc";

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
