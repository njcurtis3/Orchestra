import { stopSession } from "../ipc";
import type { SessionStatus, SessionSummary } from "../types";

const LABELS: Record<SessionStatus, string> = {
  starting: "Starting",
  working: "Working",
  waiting_permission: "Needs permission",
  waiting_question: "Has a question",
  idle: "Idle",
  ended: "Ended",
  failed: "Failed",
};

const LIVE: SessionStatus[] = ["starting", "working", "waiting_permission", "waiting_question", "idle"];

type Props = { sessions: SessionSummary[]; onError: (e: unknown) => void };

export function SessionList({ sessions, onError }: Props) {
  return (
    <section className="panel" aria-labelledby="sessions-h">
      <h2 id="sessions-h">Sessions</h2>
      {sessions.length === 0 && <p className="empty">No sessions yet.</p>}
      <ul className="list">
        {sessions.map((s) => (
          <li key={s.id} className="card">
            <div className="row">
              <strong>{s.project_name}</strong>
              <span className={`badge status-${s.status}`}>{LABELS[s.status]}</span>
            </div>
            <div className="meta">
              Last activity <time dateTime={new Date(s.last_activity_at).toISOString()}>{new Date(s.last_activity_at).toLocaleString()}</time>
            </div>
            {s.last_message && <p className="text">{s.last_message}</p>}
            {LIVE.includes(s.status) && (
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
