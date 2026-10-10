import type { InboxItem } from "../types";
import { InboxItemView } from "./InboxItemView";

type Props = { items: InboxItem[]; onError: (e: unknown) => void };

export function Inbox({ items, onError }: Props) {
  return (
    <section className="panel" aria-labelledby="inbox-h">
      <h2 id="inbox-h">Inbox</h2>
      {items.length === 0 && <p className="empty">Nothing needs you.</p>}
      <ul className="list">
        {items.map((item) => (
          <InboxItemView key={item.id} item={item} onError={onError} />
        ))}
      </ul>
    </section>
  );
}
