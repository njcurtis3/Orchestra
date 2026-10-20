// The phone's queue of answers: one entry per inbox item, pure transitions, no I/O and no timers.
// Answer text is sensitive: nothing here logs it, persists it or copies it into a message.
import type { Answer, InboxItem } from '../../../protocol/types';

export type OutboxStatus =
  | 'queued'
  | 'sending'
  | 'unconfirmed'
  | 'sent'
  | 'failed'
  | 'refused'
  | 'answered'
  | 'expired'
  | 'gone';

export type OutboxAnswer = Extract<Answer, { kind: 'permission' | 'question' }>;

export type OutboxEntry = {
  item_id: string;
  answer: OutboxAnswer;
  // One id per attempt chain: a resend after a drop reuses it so the host applies the answer once.
  call_id: string;
  // How many times this call id has gone out.
  attempt: number;
  status: OutboxStatus;
  // The host's error code, for failed and refused.
  code?: string;
  // The host's error text, for failed and refused. Never the answer.
  message?: string;
};

export type Outbox = Record<string, OutboxEntry>;

export function enqueue(answer: OutboxAnswer, callId: string): OutboxEntry {
  return { item_id: answer.item_id, answer, call_id: callId, attempt: 0, status: 'queued' };
}

// The call goes out on the live link.
export function sending(e: OutboxEntry): OutboxEntry {
  if (e.status !== 'queued') return e;
  return { ...e, status: 'sending', attempt: e.attempt + 1 };
}

// The link dropped: a call with no result goes back to the queue under the same id.
export function linkDropped(e: OutboxEntry): OutboxEntry {
  return e.status === 'sending' ? { ...e, status: 'queued' } : e;
}

// 20 s passed with no result: the answer may or may not have landed. It keeps its id.
export function timedOut(e: OutboxEntry): OutboxEntry {
  return e.status === 'sending' ? { ...e, status: 'unconfirmed' } : e;
}

function open(e: OutboxEntry): boolean {
  return e.status === 'sending' || e.status === 'unconfirmed' || e.status === 'queued';
}

export function resultOk(e: OutboxEntry): OutboxEntry {
  return open(e) ? { ...e, status: 'sent', code: undefined, message: undefined } : e;
}

// itemExpired: the phone's held copy of the item is expired. The decision on a resend is made by
// the attempt number, never by the host's text.
export function resultError(e: OutboxEntry, code: string, message: string, itemExpired: boolean): OutboxEntry {
  if (!open(e)) return e;
  if (code === 'already_answered') return { ...e, status: 'answered', code, message };
  if (code === 'forbidden') return { ...e, status: 'refused', code, message };
  if (itemExpired) return { ...e, status: 'expired', code, message };
  // A resend of the same id that comes back failed is the host's "still running" case.
  if (e.attempt > 1) return { ...e, status: 'unconfirmed', code, message };
  return { ...e, status: 'failed', code, message };
}

// Cancel is for an answer that has not gone out, or one with no reply: the user's way out of a
// replayed failure the phone cannot tell from a slow host. Null removes the entry, so a late
// result for its id matches nothing.
export function cancel(e: OutboxEntry): OutboxEntry | null {
  return e.status === 'queued' || e.status === 'unconfirmed' ? null : e;
}

// Try again. From unconfirmed the id is kept, since the first call may have landed. From failed
// the host's refusal would be replayed, so the id is new.
export function retry(e: OutboxEntry, newCallId: string): OutboxEntry {
  if (e.status === 'unconfirmed') return { ...e, status: 'queued' };
  if (e.status === 'failed') {
    return { item_id: e.item_id, answer: e.answer, call_id: newCallId, attempt: 0, status: 'queued' };
  }
  return e;
}

// Dismiss clears an entry that is over. Null removes the entry.
export function dismiss(e: OutboxEntry): OutboxEntry | null {
  return open(e) ? e : null;
}

// Compare an entry with the held inbox. Null removes the entry. A call in flight is left to its result.
export function reconcile(e: OutboxEntry, inbox: InboxItem[]): OutboxEntry | null {
  const item = inbox.find((i) => i.id === e.item_id);
  switch (e.status) {
    case 'sent':
      return !item || item.resolved || item.expired ? null : e;
    case 'queued':
    case 'unconfirmed':
    case 'failed':
      if (!item) return { ...e, status: 'gone' };
      if (item.expired) return { ...e, status: 'expired' };
      if (item.resolved) return { ...e, status: 'answered' };
      return e;
    default:
      return e;
  }
}
