import { describe, expect, it } from "vitest";
import {
  MAX_KEPT,
  canPrompt,
  costLabel,
  isLong,
  mergeTranscript,
  preview,
  promptNote,
  showsPrompt,
  usageLabel,
} from "./detail";
import { SESSION_STATUSES, type TranscriptRecord } from "./types";

const text = (seq: number, t = "x"): TranscriptRecord => ({ seq, at: seq, kind: "assistant_text", text: t, truncated: false });
const turn = (seq: number, cost: number | null, input = 1): TranscriptRecord => ({
  seq,
  at: seq,
  kind: "turn_result",
  is_error: false,
  subtype: "success",
  total_cost_usd: cost,
  duration_ms: 10,
  num_turns: 1,
  usage: { input, output: 2, cache_read: 3, cache_creation: 4 },
});

describe("canPrompt", () => {
  it("is true only for idle and suspended, across all nine statuses", () => {
    expect(SESSION_STATUSES).toHaveLength(9);
    expect(SESSION_STATUSES.filter((s) => canPrompt(s))).toEqual(["idle", "suspended"]);
  });

  it("explains every disabled status that still shows the box, and hides it for ended and failed", () => {
    for (const s of SESSION_STATUSES) {
      if (canPrompt(s)) expect(promptNote(s)).toBeNull();
      else if (showsPrompt(s)) expect(promptNote(s)).toMatch(/\S/);
      else expect(["ended", "failed"]).toContain(s);
    }
  });
});

describe("mergeTranscript", () => {
  it("dedupes by seq, keeps the copy already held, and orders ascending", () => {
    const merged = mergeTranscript([text(3), text(1)], [text(2), text(3, "dup"), text(1, "dup")]);
    expect(merged.map((r) => r.seq)).toEqual([1, 2, 3]);
    expect(merged.map((r) => (r.kind === "assistant_text" ? r.text : ""))).toEqual(["x", "x", "x"]);
  });

  it("merges events that arrived before the page, and a page that overlaps them", () => {
    const live = mergeTranscript([], [text(5), text(6)]);
    expect(mergeTranscript(live, [text(1), text(2), text(5)]).map((r) => r.seq)).toEqual([1, 2, 5, 6]);
  });

  it("does not mutate its inputs and keeps only the newest entries past the cap", () => {
    const a = [text(2)];
    mergeTranscript(a, [text(1)]);
    expect(a).toHaveLength(1);
    const many = Array.from({ length: MAX_KEPT + 3 }, (_, i) => text(i + 1));
    const kept = mergeTranscript([], many);
    expect([kept.length, kept[0].seq]).toEqual([MAX_KEPT, 4]);
  });
});

describe("costLabel", () => {
  it("uses the last turn result, not a sum", () => {
    const label = costLabel([turn(1, 0.002), text(2), turn(3, 0.004)]);
    expect(label).toContain("$0.0040");
    expect(label).not.toContain("0.0060");
    expect(label).toContain("reported by claude");
  });

  it("falls back to the last reported cost, and says so when there is none", () => {
    expect(costLabel([turn(1, 0.002), turn(2, null)])).toContain("$0.0020");
    expect(costLabel([text(1)])).toBe("No cost reported yet");
  });

  it("reports the tokens of the last turn", () => {
    expect(usageLabel([turn(1, 0.1, 7), turn(2, 0.2, 9)])).toBe("Last turn: 9 in, 2 out, 3 cache read, 4 cache write");
    expect(usageLabel([text(1)])).toBeNull();
  });
});

describe("collapsing", () => {
  it("collapses long text and previews on one line", () => {
    expect(isLong("a".repeat(401))).toBe(true);
    expect(isLong("short")).toBe(false);
    expect(preview("a\n\nb")).toBe("a b");
    expect(preview("z".repeat(500)).endsWith("...")).toBe(true);
  });
});
