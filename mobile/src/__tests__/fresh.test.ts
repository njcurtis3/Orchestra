import { FreshTracker, isPending } from '../link/fresh';
import { inboxItem } from './fake-host';
import type { InboxItem } from '../../../protocol/types';

const item = (id: string, over: Partial<InboxItem> = {}): InboxItem => ({ ...(inboxItem(id) as InboxItem), ...over });

describe('FreshTracker', () => {
  it('a snapshot with N pending items gives 0 fresh', () => {
    const t = new FreshTracker();
    expect(t.seed([item('a'), item('b'), item('c', { kind: 'question' })])).toEqual({ fresh: [], gone: [] });
  });

  it('an update adding one id gives exactly that item, once', () => {
    const t = new FreshTracker();
    t.seed([item('a')]);
    expect(t.update([item('a'), item('b')]).fresh.map((i) => i.id)).toEqual(['b']);
    expect(t.update([item('a'), item('b')]).fresh).toEqual([]);
  });

  it('never gives resolved, expired, finished or error items', () => {
    const t = new FreshTracker();
    t.seed([]);
    const r = t.update([
      item('r', { resolved: true }),
      item('x', { expired: true }),
      item('f', { kind: 'finished' }),
      item('e', { kind: 'error' }),
    ]);
    expect(r.fresh).toEqual([]);
  });

  it('reports notified ids that are no longer pending, once', () => {
    const t = new FreshTracker();
    t.seed([]);
    t.update([item('a')]);
    expect(t.update([item('a', { resolved: true })]).gone).toEqual(['a']);
    expect(t.update([item('a', { resolved: true })]).gone).toEqual([]);
  });

  it('a reseed makes earlier items known, so they are not fresh later', () => {
    const t = new FreshTracker();
    t.seed([]);
    t.seed([item('a')]);
    expect(t.update([item('a')]).fresh).toEqual([]);
  });

  it('clear empties the set and returns the notified ids', () => {
    const t = new FreshTracker();
    t.seed([]);
    t.update([item('a')]);
    expect(t.clear()).toEqual(['a']);
    expect(t.update([item('a')]).fresh.map((i) => i.id)).toEqual(['a']);
  });

  it('isPending is permission or question, not resolved, not expired', () => {
    expect(isPending(item('a'))).toBe(true);
    expect(isPending(item('a', { kind: 'question' }))).toBe(true);
    expect(isPending(item('a', { kind: 'finished' }))).toBe(false);
  });
});
