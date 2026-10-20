import type { InboxItem } from '../../../protocol/types';
import * as box from '../link/outbox';
import type { OutboxAnswer } from '../link/outbox';
import { inboxItem } from './fake-host';

const answer: OutboxAnswer = { kind: 'permission', item_id: 'i1', decision: 'allow' };
const item = (over: Partial<InboxItem> = {}): InboxItem => ({ ...inboxItem('i1'), ...over }) as InboxItem;
const queued = () => box.enqueue(answer, 'a-1');
const sent = () => box.sending(queued());

describe('outbox transitions', () => {
  it('enqueue starts queued with attempt 0, sending counts the attempt', () => {
    expect(queued()).toMatchObject({ item_id: 'i1', call_id: 'a-1', attempt: 0, status: 'queued' });
    expect(sent()).toMatchObject({ status: 'sending', attempt: 1 });
  });

  it('a drop turns sending into queued under the same id, other statuses stay', () => {
    expect(box.linkDropped(sent())).toMatchObject({ status: 'queued', call_id: 'a-1', attempt: 1 });
    const u = box.timedOut(sent());
    expect(box.linkDropped(u)).toBe(u);
  });

  it('a timeout makes sending unconfirmed and keeps the id', () => {
    expect(box.timedOut(sent())).toMatchObject({ status: 'unconfirmed', call_id: 'a-1' });
  });

  it('a late ok lands on an unconfirmed entry', () => {
    expect(box.resultOk(box.timedOut(sent())).status).toBe('sent');
  });

  it('maps host errors', () => {
    expect(box.resultError(sent(), 'already_answered', 'x', false).status).toBe('answered');
    expect(box.resultError(sent(), 'forbidden', 'x', false).status).toBe('refused');
    expect(box.resultError(sent(), 'failed', 'x', true).status).toBe('expired');
    expect(box.resultError(sent(), 'failed', 'boom', false)).toMatchObject({ status: 'failed', message: 'boom' });
    expect(box.resultError(sent(), 'bad_request', 'x', false)).toMatchObject({ status: 'failed', code: 'bad_request' });
  });

  it('failed on a resend of the same id is unconfirmed, whatever the text says', () => {
    const resend = box.sending(box.linkDropped(sent()));
    expect(resend.attempt).toBe(2);
    expect(box.resultError(resend, 'failed', 'anything at all', false).status).toBe('unconfirmed');
  });

  it('cancel removes only a queued or unconfirmed entry', () => {
    expect(box.cancel(queued())).toBeNull();
    expect(box.cancel(box.timedOut(box.sending(queued())))).toBeNull();
    const s = sent();
    expect(box.cancel(s)).toBe(s);
  });

  it('retry from unconfirmed keeps the id, from failed takes a new one', () => {
    const u = box.timedOut(sent());
    expect(box.retry(u, 'a-2')).toMatchObject({ status: 'queued', call_id: 'a-1', attempt: 1 });
    const f = box.resultError(sent(), 'failed', 'x', false);
    const again = box.retry(f, 'a-2');
    expect(again).toMatchObject({ status: 'queued', call_id: 'a-2', attempt: 0 });
    expect(again.message).toBeUndefined();
    const a = box.resultError(sent(), 'already_answered', 'x', false);
    expect(box.retry(a, 'a-3')).toBe(a);
  });

  it('dismiss clears an entry that is over and keeps one that is open', () => {
    expect(box.dismiss(box.resultError(sent(), 'forbidden', 'x', false))).toBeNull();
    const s = sent();
    expect(box.dismiss(s)).toBe(s);
  });
});

describe('reconcile', () => {
  it('a queued answer whose item is resolved becomes answered', () => {
    expect(box.reconcile(queued(), [item({ resolved: true })])?.status).toBe('answered');
  });

  it('a queued answer whose item is expired becomes expired', () => {
    expect(box.reconcile(queued(), [item({ resolved: true, expired: true })])?.status).toBe('expired');
  });

  it('a queued answer whose item is missing becomes gone', () => {
    expect(box.reconcile(queued(), [])?.status).toBe('gone');
  });

  it('an open item leaves the entry as it is', () => {
    const q = queued();
    expect(box.reconcile(q, [item()])).toBe(q);
  });

  it('an expired failed entry becomes expired, so it never retries', () => {
    const f = box.resultError(sent(), 'failed', 'x', false);
    const e = box.reconcile(f, [item({ expired: true })]);
    expect(e?.status).toBe('expired');
    expect(box.retry(e!, 'a-9')).toBe(e);
  });

  it('sent is cleared once the item resolves or goes, kept while open', () => {
    const s = box.resultOk(sent());
    expect(box.reconcile(s, [item()])).toBe(s);
    expect(box.reconcile(s, [item({ resolved: true })])).toBeNull();
    expect(box.reconcile(s, [])).toBeNull();
  });

  it('a call in flight is left to its result', () => {
    const s = sent();
    expect(box.reconcile(s, [])).toBe(s);
  });
});
