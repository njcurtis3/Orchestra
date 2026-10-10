import { stopSession } from "../ipc";
import type { PermissionMode, SessionStatus, SessionSummary } from "../types";

export const MODE_LABELS: Record<PermissionMode, string> = {
  manual: "manual",
  acceptEdits: "accept edits",
  plan: "plan",
};

export const STATUS_LABELS: Record<SessionStatus, string> = {
  starting: "Starting",
  working: "Working",
  waiting_permission: "Needs permission",
  waiting_question: "Has a question",
  needs_attention: "Needs attention",
  idle: "Idle",
  suspended: "Paused",
  ended: "Ended",
  failed: "Failed",
};

// Record, not a list: tsc fails if a new status is added without deciding whether Stop shows.
const LIVE: Record<SessionStatus, boolean> = {
  starting: true,
  working: true,
  waiting_permission: true,
  waiting_question: true,
  needs_attention: true,
  idle: true,
  suspended: false,
  ended: false,
  failed: false,
};

type Props = {
  sessions: SessionSummary[];
  selected: string | null;
  onSelect: (id: string) => void;
  onError: (e: unknown) => void;
};

export function SessionList({ sessions, selected, onSelect, onError }: Props) {
  return (
    <section className="panel" aria-labelledby="sessions-h">
      <h2 id="sessions-h">Sessions</h2>
      {sessions.length === 0 && <p className="empty">No sessions yet.</p>}
      <ul className="list">
        {sessions.map((s) => (
          <li key={s.id} className={`card${s.id === selected ? " selected" : ""}`}>
            <div className="row">
              <button type="button" className="linklike" aria-current={s.id === selected} onClick={() => onSelect(s.id)}>
                {s.project_name}
              </button>
              <span className={`badge status-${s.status}`}>{STATUS_LABELS[s.status]}</span>
            </div>
            <div className="meta">
              Last activity <time dateTime={new Date(s.last_activity_at).toISOString()}>{new Date(s.last_activity_at).toLocaleString()}</time>
            </div>
            {(s.model || s.permission_mode) && (
              <div className="meta">
                {[s.model, s.permission_mode && MODE_LABELS[s.permission_mode]].filter(Boolean).join(" · ")}
              </div>
            )}
            {s.last_message && <p className="text">{s.last_message}</p>}
            {LIVE[s.status] && (
              <button type="button" onClick={() => stopSession(s.id).catch(onError)}>
                Stop
              </button>
            )}
          </li>
        ))}
      </ul>
    </section>
  );
}
