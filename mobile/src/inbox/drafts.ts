// Unsent picks and typed text, by item id, so Back and reopen lose nothing (A2-G10).
// In memory only: draft text is sensitive and is never written to disk or logged.
import { useCallback, useState } from 'react';

import { emptyQuestionDraft, type QuestionDraft } from './answer';

export type Draft = {
  // The optional deny message of a permission item.
  message: string;
  question: QuestionDraft;
};

const drafts = new Map<string, Draft>();

const blank = (): Draft => ({ message: '', question: emptyQuestionDraft() });

export function getDraft(itemId: string): Draft {
  return drafts.get(itemId) ?? blank();
}

export function setDraft(itemId: string, draft: Draft): void {
  drafts.set(itemId, draft);
}

export function clearDrafts(): void {
  drafts.clear();
}

// The draft of one item. Every change is written through to the store before the next render.
export function useDraft(itemId: string): [Draft, (update: (d: Draft) => Draft) => void] {
  const [draft, setState] = useState(() => getDraft(itemId));
  const change = useCallback(
    (update: (d: Draft) => Draft) => {
      const next = update(getDraft(itemId));
      setDraft(itemId, next);
      setState(next);
    },
    [itemId],
  );
  return [draft, change];
}
