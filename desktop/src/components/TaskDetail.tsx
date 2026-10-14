import { useEffect, useState } from "react";
import { discardTask, getTaskDiff, landTask, retryCleanup } from "../ipc";
import { TASK_STATUS_LABELS, canDiscard, canLand, canRetryCleanup, landSummary } from "../tasks";
import type { TaskDiff, TaskSummary } from "../types";
const errText = (e: unknown) => (e instanceof Error ? e.message : String(e));
import { DiffView } from "./DiffView";

type Props = {
  task: TaskSummary;
  // Whether the task's session is known to the session list, so Open session can show it.
  hasSession: boolean;
  onOpenSession: (sessionId: string) => void;
  onTask: (t: TaskSummary) => void;
  onBack: () => void;
};

type Confirm = "land" | "discard" | null;

// Keyed by task id in App, so the diff, the confirm and the in-flight flag never carry
// from one task to the next. Every string shown here (title, note, commit text, patch) is
// untrusted and rendered as a plain text node.
export function TaskDetail({ task, hasSession, onOpenSession, onTask, onBack }: Props) {
  const [diff, setDiff] = useState<TaskDiff | null>(null);
  const [diffError, setDiffError] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<Confirm>(null);
  const [busy, setBusy] = useState(false);
  // A Rust refusal (dirty checkout, conflict, Claude attribution in a commit) stays visible
  // next to the buttons until the owner tries again or leaves.
  const [refusal, setRefusal] = useState<string | null>(null);

  const id = task.id;
  const reviewable = task.status === "done";

  useEffect(() => {
    let cancelled = false;
    setDiff(null);
    setDiffError(null);
    if (!reviewable) return;
    void getTaskDiff(id)
      .then((d) => {
        if (!cancelled) setDiff(d);
      })
      .catch((e) => {
        if (!cancelled) setDiffError(errText(e));
      });
    return () => {
      cancelled = true;
    };
  }, [id, reviewable]);

  async function act(call: () => Promise<TaskSummary>) {
    if (busy) return;
    setBusy(true);
    setRefusal(null);
    try {
      onTask(await call());
      setConfirm(null);
    } catch (e) {
      setConfirm(null);
      setRefusal(errText(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="panel detail" aria-labelledby="task-h">
      <div className="row">
        <h2 id="task-h">{task.title || "(no title)"}</h2>
        <button type="button" onClick={onBack}>
          Back to inbox
        </button>
      </div>
      <div className="meta">
        <span className={`badge task-${task.status}`}>{TASK_STATUS_LABELS[task.status]}</span>{" "}
        {task.project_name} · {task.branch ?? "no branch yet"} · base {task.base_branch}
      </div>
      {task.note && <p className="text">{task.note}</p>}
      {task.session_over && <p className="meta">The session has ended; no follow-up is possible.</p>}
      {task.last_turn_error && <p className="meta">The last turn ended in an error.</p>}
      {task.session_id && hasSession && (
        <p>
          <button type="button" onClick={() => onOpenSession(task.session_id as string)}>
            Open session
          </button>
        </p>
      )}

      {refusal && (
        <div className="banner" role="alert">
          <span>{refusal}</span>
          <button type="button" onClick={() => setRefusal(null)}>
            Dismiss
          </button>
        </div>
      )}

      {reviewable && (
        <div className="review">
          <h3>Review</h3>
          {diffError && <p className="hint">{diffError}</p>}
          {!diff && !diffError && <p className="empty">Reading the changes…</p>}
          {diff && (
            <>
              <h4>Commits ({diff.commits.length})</h4>
              {diff.commits.length === 0 && <p className="empty">No commits; changes are uncommitted in the worktree.</p>}
              <ul className="list">
                {diff.commits.map((c) => (
                  <li key={c.id} className="card">
                    <div>
                      <code>{c.id}</code> {c.subject}
                    </div>
                    {c.body && <pre className="commit-body">{c.body}</pre>}
                  </li>
                ))}
              </ul>
              <h4>Files ({diff.files.length}{diff.files_truncated ? "+" : ""})</h4>
              {diff.files_truncated && <p className="hint">File list truncated; more files changed than are listed.</p>}
              <ul className="list files">
                {diff.files.map((f) => (
                  <li key={f.path} className="row">
                    <span>
                      {f.path} <span className="meta">({f.status})</span>
                    </span>
                    <span className="meta">{f.binary ? "binary" : `+${f.added} -${f.removed}`}</span>
                  </li>
                ))}
              </ul>
              <h4>Diff</h4>
              <DiffView patch={diff.patch} truncated={diff.truncated} />
            </>
          )}
        </div>
      )}

      {confirm === "land" && (
        <div className="card confirm" role="alertdialog" aria-labelledby="confirm-h">
          <h3 id="confirm-h">Land this task?</h3>
          <p>{landSummary(task, diff?.files.length ?? 0)}</p>
          <p className="meta">The worktree and branch are removed afterwards.</p>
          <div className="actions">
            <button type="button" disabled={busy} onClick={() => act(() => landTask(id))}>
              Confirm land
            </button>
            <button type="button" disabled={busy} onClick={() => setConfirm(null)}>
              Cancel
            </button>
          </div>
        </div>
      )}
      {confirm === "discard" && (
        <div className="card confirm" role="alertdialog" aria-labelledby="confirm-h">
          <h3 id="confirm-h">Discard this task?</h3>
          <p>
            Delete the worktree and branch {task.branch ?? ""} of {task.project_name}. Its changes are lost.
          </p>
          <div className="actions">
            <button type="button" disabled={busy} onClick={() => act(() => discardTask(id))}>
              Confirm discard
            </button>
            <button type="button" disabled={busy} onClick={() => setConfirm(null)}>
              Cancel
            </button>
          </div>
        </div>
      )}

      {confirm === null && (
        <div className="actions">
          {canLand(task) && (
            <button type="button" disabled={busy || !diff} onClick={() => setConfirm("land")}>
              Land
            </button>
          )}
          {canDiscard(task) && (
            <button type="button" disabled={busy} onClick={() => setConfirm("discard")}>
              Discard
            </button>
          )}
          {canRetryCleanup(task) && (
            <button type="button" disabled={busy} onClick={() => act(() => retryCleanup(id))}>
              Retry cleanup
            </button>
          )}
        </div>
      )}
    </section>
  );
}
