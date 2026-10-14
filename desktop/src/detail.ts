import type { SessionStatus, TranscriptRecord } from "./types";

// Record, not a list: tsc fails if a new status is added without deciding what the prompt box does.
// `can` enables it; `note` is the visible reason when it is disabled; `shown: false` removes it.
const PROMPT: Record<SessionStatus, { can: boolean; shown: boolean; note: string | null }> = {
  starting: { can: false, shown: true, note: "Wait for the session to start." },
  working: { can: false, shown: true, note: "Claude is working. You can reply when the turn ends." },
  waiting_permission: { can: false, shown: true, note: "Answer the permission request in the inbox first." },
  waiting_question: { can: false, shown: true, note: "Answer the question in the inbox first." },
  needs_attention: { can: false, shown: true, note: "The session needs attention. Stop it, or wait for it to continue." },
  idle: { can: true, shown: true, note: null },
  suspended: { can: true, shown: true, note: null },
  ended: { can: false, shown: false, note: null },
  failed: { can: false, shown: false, note: null },
};

// Only an idle session takes a follow-up, and a paused one resumes with it. The Rust side
// refuses anything else, so this only decides what the box looks like.
export const canPrompt = (status: SessionStatus): boolean => PROMPT[status].can;
export const showsPrompt = (status: SessionStatus): boolean => PROMPT[status].shown;
export const promptNote = (status: SessionStatus): string | null => PROMPT[status].note;

// Keep this many of the newest entries in memory.
export const MAX_KEPT = 2000;

// Union by seq, ascending. Subscribing before loading means the same entry can arrive twice;
// the copy already held wins, and a seq is never shown twice.
export function mergeTranscript(current: TranscriptRecord[], incoming: TranscriptRecord[]): TranscriptRecord[] {
  const bySeq = new Map(current.map((r) => [r.seq, r]));
  for (const r of incoming) if (!bySeq.has(r.seq)) bySeq.set(r.seq, r);
  const merged = [...bySeq.values()].sort((a, b) => a.seq - b.seq);
  return merged.length > MAX_KEPT ? merged.slice(merged.length - MAX_KEPT) : merged;
}

type Turn = Extract<TranscriptRecord, { kind: "turn_result" }>;

// The newest turn result that carries a cost. claude reports the session total on each one,
// so the latest is the answer and adding them up would count earlier turns again.
export function latestCostTurn(entries: TranscriptRecord[]): Turn | null {
  for (let i = entries.length - 1; i >= 0; i--) {
    const e = entries[i];
    if (e.kind === "turn_result" && e.total_cost_usd !== null) return e;
  }
  return null;
}

export function latestTurn(entries: TranscriptRecord[]): Turn | null {
  for (let i = entries.length - 1; i >= 0; i--) {
    const e = entries[i];
    if (e.kind === "turn_result") return e;
  }
  return null;
}

export function costLabel(entries: TranscriptRecord[]): string {
  const t = latestCostTurn(entries);
  return t && t.total_cost_usd !== null
    ? `Session total reported by claude: $${t.total_cost_usd.toFixed(4)}`
    : "No cost reported yet";
}

export function usageLabel(entries: TranscriptRecord[]): string | null {
  const t = latestTurn(entries);
  if (!t || !t.usage) return null;
  const u = t.usage;
  return `Last turn: ${u.input} in, ${u.output} out, ${u.cache_read} cache read, ${u.cache_creation} cache write`;
}

// Fields longer than this start collapsed.
export const COLLAPSE_AT = 400;
export const PREVIEW_CHARS = 120;

export const isLong = (text: string): boolean => text.length > COLLAPSE_AT;
export const preview = (text: string): string =>
  text.slice(0, PREVIEW_CHARS).replace(/\s+/g, " ") + (text.length > PREVIEW_CHARS ? "..." : "");
