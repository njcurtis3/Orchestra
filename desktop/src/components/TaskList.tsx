import { useState } from "react";
import { cancelTask, retryCleanup } from "../ipc";
import {
  TASK_GROUPS,
  TASK_GROUP_LABELS,
  TASK_STATUS_LABELS,
  canCancel,
  canRetryCleanup,
  groupTasks,
} from "../tasks";
import type { TaskSummary } from "../types";

type Props = {
  tasks: TaskSummary[];
  selected: string | null;
  onSelect: (id: string) => void;
  onTask: (t: TaskSummary) => void;
  onError: (e: unknown) => void;
};

export function TaskList({ tasks, selected, onSelect, onTask, onError }: Props) {
  // The task with a call in flight; its buttons are disabled until the call settles.
  const [busy, setBusy] = useState<string | null>(null);
  const groups = groupTasks(tasks);

  async function run(id: string, call: () => Promise<TaskSummary | void>) {
    if (busy) return;
    setBusy(id);
    try {
      const t = await call();
      if (t) onTask(t);
    } catch (e) {
      onError(e);
    } finally {
      setBusy(null);
    }
  }

  const card = (t: TaskSummary) => (
    <li key={t.id} className={`card${t.id === selected ? " selected" : ""}`}>
      <div className="row">
        <button type="button" className="linklike" aria-current={t.id === selected} onClick={() => onSelect(t.id)}>
          {t.title || "(no title)"}
        </button>
        <span className={`badge task-${t.status}`}>{TASK_STATUS_LABELS[t.status]}</span>
      </div>
      <div className="meta">
        {t.project_name}
        {t.branch ? ` · ${t.branch}` : ""}
      </div>
      {t.note && <p className="text">{t.note}</p>}
      <div className="actions">
        {canCancel(t) && (
          <button type="button" disabled={busy !== null} onClick={() => run(t.id, () => cancelTask(t.id))}>
            Cancel
          </button>
        )}
        {canRetryCleanup(t) && (
          <button type="button" disabled={busy !== null} onClick={() => run(t.id, () => retryCleanup(t.id))}>
            Retry cleanup
          </button>
        )}
        {!canCancel(t) && !canRetryCleanup(t) && (
          <button type="button" onClick={() => onSelect(t.id)}>
            Open
          </button>
        )}
      </div>
    </li>
  );

  return (
    <section className="panel" aria-labelledby="tasks-h">
      <h2 id="tasks-h">Tasks</h2>
      {tasks.length === 0 && <p className="empty">No tasks yet.</p>}
      {TASK_GROUPS.map((g) => {
        const items = groups[g];
        if (items.length === 0) return null;
        if (g === "closed") {
          return (
            <details key={g} className="task-group">
              <summary>
                {TASK_GROUP_LABELS[g]} ({items.length})
              </summary>
              <ul className="list">{items.map(card)}</ul>
            </details>
          );
        }
        return (
          <div key={g} className="task-group">
            <h3>
              {TASK_GROUP_LABELS[g]} ({items.length})
            </h3>
            <ul className="list">{items.map(card)}</ul>
          </div>
        );
      })}
    </section>
  );
}
