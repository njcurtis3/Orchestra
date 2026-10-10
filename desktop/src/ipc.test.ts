import { describe, expect, it } from "vitest";
import { decodeSession } from "./ipc";

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
