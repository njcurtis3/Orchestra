// Which inbox items are new since the last snapshot, and which notified ones have gone. Pure: no I/O, no timers.
// A snapshot only seeds; nothing is fresh at connect or reconnect. Only a later inbox-updated can be fresh.
import type { InboxItem } from '../../../protocol/types';

export type FreshResult = { fresh: InboxItem[]; gone: string[] };

export const isPending = (i: InboxItem): boolean =>
  (i.kind === 'permission' || i.kind === 'question') && !i.resolved && !i.expired;

export class FreshTracker {
  // Every id in the current inbox, whatever its kind and state.
  private known = new Set<string>();
  // Ids a notification was asked for and that are still pending.
  private notified = new Set<string>();

  // The snapshot of a connection: remember what is there, notify nothing.
  seed(inbox: InboxItem[]): FreshResult {
    this.known = new Set(inbox.map((i) => i.id));
    return { fresh: [], gone: this.drop(inbox) };
  }

  // The inbox after an inbox-updated: items not seen before and still pending are fresh.
  update(inbox: InboxItem[]): FreshResult {
    const fresh = inbox.filter((i) => isPending(i) && !this.known.has(i.id));
    this.known = new Set(inbox.map((i) => i.id));
    for (const i of fresh) this.notified.add(i.id);
    return { fresh, gone: this.drop(inbox) };
  }

  // Forget and revoke: nothing is known, and every notified id is returned so it can be dismissed.
  clear(): string[] {
    const all = [...this.notified];
    this.known = new Set();
    this.notified = new Set();
    return all;
  }

  private drop(inbox: InboxItem[]): string[] {
    const pending = new Set(inbox.filter(isPending).map((i) => i.id));
    const gone = [...this.notified].filter((id) => !pending.has(id));
    for (const id of gone) this.notified.delete(id);
    return gone;
  }
}
