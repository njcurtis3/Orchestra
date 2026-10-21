import { describe, expect, it } from "vitest";
import { MAX_ANSWERS, MAX_ANSWER_BYTES, answersTooLarge } from "./answers";

describe("answersTooLarge", () => {
  it("allows a value of exactly 4096 bytes and refuses 4097", () => {
    expect(answersTooLarge({ q: "a".repeat(MAX_ANSWER_BYTES) })).toBe(false);
    expect(answersTooLarge({ q: "a".repeat(MAX_ANSWER_BYTES + 1) })).toBe(true);
  });

  it("counts UTF-8 bytes, not characters", () => {
    // 3 bytes each: 1365 chars = 4095 bytes, 1366 = 4098.
    expect(answersTooLarge({ q: "€".repeat(1365) })).toBe(false);
    expect(answersTooLarge({ q: "€".repeat(1366) })).toBe(true);
  });

  it("refuses a key over 4096 bytes", () => {
    expect(answersTooLarge({ ["k".repeat(MAX_ANSWER_BYTES)]: "x" })).toBe(false);
    expect(answersTooLarge({ ["k".repeat(MAX_ANSWER_BYTES + 1)]: "x" })).toBe(true);
  });

  it("allows 32 keys and refuses 33", () => {
    const make = (n: number) => Object.fromEntries(Array.from({ length: n }, (_, i) => [`q${i}`, "a"]));
    expect(answersTooLarge(make(MAX_ANSWERS))).toBe(false);
    expect(answersTooLarge(make(MAX_ANSWERS + 1))).toBe(true);
  });

  it("allows no answers", () => {
    expect(answersTooLarge({})).toBe(false);
  });
});
