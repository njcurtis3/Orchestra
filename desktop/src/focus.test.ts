import { describe, expect, it } from "vitest";
import { cycleFocus } from "./focus";

describe("cycleFocus", () => {
  it("has nothing to focus when the list is empty", () => {
    expect(cycleFocus(0, -1, false)).toBe(-1);
    expect(cycleFocus(0, -1, true)).toBe(-1);
  });

  it("stays on the only control", () => {
    expect(cycleFocus(1, 0, false)).toBe(0);
    expect(cycleFocus(1, 0, true)).toBe(0);
  });

  it("wraps at both ends and steps in between", () => {
    expect(cycleFocus(3, 2, false)).toBe(0);
    expect(cycleFocus(3, 0, true)).toBe(2);
    expect(cycleFocus(3, 0, false)).toBe(1);
    expect(cycleFocus(3, 1, true)).toBe(0);
  });

  it("enters the list from the dialog itself", () => {
    expect(cycleFocus(3, -1, false)).toBe(0);
    expect(cycleFocus(3, -1, true)).toBe(2);
  });
});
