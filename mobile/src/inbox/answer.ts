// Pure answer builders for the item screens. They mirror desktop/src/components/InboxItemView.tsx,
// and the limits copy the host's (core/src/session/mod.rs): the host stays the authority.
// Answer text is sensitive: nothing here logs it or copies it into a message.
import type { Answer, InboxItem, Question } from '../../../protocol/types';
import type { Outbox } from '@/link/outbox';

export const MAX_ANSWERS = 32;
export const MAX_ANSWER_BYTES = 4096;
export const MAX_TEXT_BYTES = 32768;

// UTF-8 length without TextEncoder, which older Hermes builds lack.
export function byteLength(s: string): number {
  let n = 0;
  for (const ch of s) {
    const c = ch.codePointAt(0) as number;
    n += c < 0x80 ? 1 : c < 0x800 ? 2 : c < 0x10000 ? 3 : 4;
  }
  return n;
}

export type PermissionAnswer = Extract<Answer, { kind: 'permission' }>;
export type QuestionAnswer = Extract<Answer, { kind: 'question' }>;

export function allowAnswer(itemId: string): PermissionAnswer {
  return { kind: 'permission', item_id: itemId, decision: 'allow' };
}

// The message goes out only when it has text besides spaces, and then as typed, as the desktop does.
export function denyAnswer(itemId: string, message: string): PermissionAnswer {
  return {
    kind: 'permission',
    item_id: itemId,
    decision: 'deny',
    ...(message.trim() ? { message } : {}),
  };
}

export function denyTooLong(message: string): boolean {
  return byteLength(message) > MAX_TEXT_BYTES;
}

// Picked option labels and Other text, both by question text.
export type QuestionDraft = {
  picks: Record<string, string[]>;
  other: Record<string, string>;
};

export const emptyQuestionDraft = (): QuestionDraft => ({ picks: {}, other: {} });

// A single-select question holds one pick; a multi-select one toggles.
export function togglePick(
  draft: QuestionDraft,
  q: Question,
  label: string,
  checked: boolean,
): QuestionDraft {
  const cur = draft.picks[q.question] ?? [];
  const next = q.multi_select ? (checked ? [...cur, label] : cur.filter((l) => l !== label)) : [label];
  return { ...draft, picks: { ...draft.picks, [q.question]: next } };
}

export function setOther(draft: QuestionDraft, q: Question, text: string): QuestionDraft {
  return { ...draft, other: { ...draft.other, [q.question]: text } };
}

// Picks and the trimmed Other text, joined with ", " and keyed by question text.
export function collect(questions: Question[], draft: QuestionDraft): Record<string, string> {
  const out: Record<string, string> = {};
  for (const q of questions) {
    const parts = [...(draft.picks[q.question] ?? [])];
    const o = (draft.other[q.question] ?? '').trim();
    if (o) parts.push(o);
    if (parts.length) out[q.question] = parts.join(', ');
  }
  return out;
}

export function isComplete(questions: Question[], answers: Record<string, string>): boolean {
  return questions.length > 0 && questions.every((q) => q.question in answers);
}

export type QuestionLimit = 'ok' | 'too_many' | 'too_long';

export function questionLimit(questions: Question[], answers: Record<string, string>): QuestionLimit {
  if (questions.length > MAX_ANSWERS || Object.keys(answers).length > MAX_ANSWERS) return 'too_many';
  for (const q of questions) {
    if (byteLength(q.question) > MAX_ANSWER_BYTES) return 'too_long';
  }
  for (const [k, v] of Object.entries(answers)) {
    if (byteLength(k) > MAX_ANSWER_BYTES || byteLength(v) > MAX_ANSWER_BYTES) return 'too_long';
  }
  return 'ok';
}

export function questionAnswer(itemId: string, answers: Record<string, string>): QuestionAnswer {
  return { kind: 'question', item_id: itemId, answers };
}

// Rebuilds a draft from a held question answer, so a queued or failed one shows what was sent.
// Labels are matched longest first, so one that contains a comma still lands; what is left is Other.
export function draftFromAnswer(questions: Question[], answers: Record<string, string>): QuestionDraft {
  const draft = emptyQuestionDraft();
  for (const q of questions) {
    const value = answers[q.question];
    if (value === undefined) continue;
    const labels = [...new Set(q.options.map((o) => o.label))].sort((a, b) => b.length - a.length);
    const max = q.multi_select ? labels.length : 1;
    let rest = value;
    const picks: string[] = [];
    let found = true;
    while (found && picks.length < max) {
      found = false;
      for (const label of labels) {
        if (picks.includes(label)) continue;
        if (rest === label) {
          picks.push(label);
          rest = '';
          found = true;
          break;
        }
        if (rest.startsWith(`${label}, `)) {
          picks.push(label);
          rest = rest.slice(label.length + 2);
          found = true;
          break;
        }
      }
    }
    draft.picks[q.question] = picks;
    if (rest) draft.other[q.question] = rest;
  }
  return draft;
}

// Outcomes that stay on the list until dismissed, even after the item itself is no longer pending.
const NOT_SENT = ['failed', 'refused', 'answered', 'expired', 'gone'];

export const isNotSent = (status: string): boolean => NOT_SENT.includes(status);

// What home lists: pending permission and question items, plus any item holding a not-sent
// outcome, newest first.
export function listedItems(inbox: InboxItem[], outbox: Outbox): InboxItem[] {
  return inbox
    .filter((i) => {
      if (i.kind !== 'permission' && i.kind !== 'question') return false;
      if (!i.resolved && !i.expired) return true;
      const e = outbox[i.id];
      return !!e && isNotSent(e.status);
    })
    .sort((a, b) => b.created_at - a.created_at);
}
