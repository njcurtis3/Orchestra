// What reaches the notification shade. Pure. Only the kind, the project name and a short summary leave the app;
// never a tool name, tool input, question, result text, host address, key, answer or draft.
import type { InboxItem } from '../../../protocol/types';

export const BODY_MAX = 100;
export const ID_MAX = 128;
export const BODY_FALLBACK = 'Open Orchestra to answer.';
export const UNKNOWN_PROJECT = 'Unknown project';

export type NotifyData = { id: string; kind: string };
export type NotifyPayload = {
  identifier: string;
  content: { title: string; body: string; data: NotifyData };
};

function body(summary: string): string {
  const s = summary.replace(/\s+/g, ' ').trim();
  if (!s) return BODY_FALLBACK;
  const chars = Array.from(s);
  // Cut on a code point, not a UTF-16 unit, so no lone surrogate is left before the ellipsis.
  return chars.length > BODY_MAX ? chars.slice(0, BODY_MAX - 1).join('').trimEnd() + '…' : s;
}

export function buildPayload(item: InboxItem, projectName: string | null | undefined): NotifyPayload {
  const kind = item.kind === 'question' ? 'Question' : 'Permission';
  const project = projectName?.trim() || UNKNOWN_PROJECT;
  return {
    // The item id, so a dismiss finds it and a repeat replaces it.
    identifier: item.id,
    content: { title: `${kind} from ${project}`, body: body(item.summary ?? ''), data: { id: item.id, kind: item.kind } },
  };
}

// The item id from a tapped notification's data, or null when it is not a usable id.
export function parseTap(data: unknown): string | null {
  if (typeof data !== 'object' || data === null) return null;
  const id = (data as Record<string, unknown>).id;
  return typeof id === 'string' && id.length > 0 && id.length <= ID_MAX ? id : null;
}
