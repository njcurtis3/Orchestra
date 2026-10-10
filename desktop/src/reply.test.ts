import { describe, expect, it } from "vitest";
import { replyThenClear } from "./reply";
import type { Answer } from "./types";

const reply: Answer = { kind: "reply", session_id: "s1", text: "hi" };

describe("replyThenClear", () => {
  it("clears only on success", async () => {
    let cleared = 0;
    await replyThenClear(async () => true, reply, () => cleared++);
    expect(cleared).toBe(1);
  });

  it("keeps the text when the send fails", async () => {
    let cleared = 0;
    await replyThenClear(async () => false, reply, () => cleared++);
    expect(cleared).toBe(0);
  });
});
