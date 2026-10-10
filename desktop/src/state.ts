import type { InboxItem, SessionSummary } from "./types";

export type State = { sessions: SessionSummary[]; inbox: InboxItem[] };

export type Action =
  | { type: "sessions-loaded"; sessions: SessionSummary[] }
  | { type: "session-updated"; session: SessionSummary }
  | { type: "inbox-updated"; items: InboxItem[] };

export const initialState: State = { sessions: [], inbox: [] };

// Unresolved first, then newest first.
export function sortInbox(items: InboxItem[]): InboxItem[] {
  return [...items].sort((a, b) =>
    a.resolved !== b.resolved ? (a.resolved ? 1 : -1) : b.created_at - a.created_at,
  );
}

// Most recent activity first.
export function sortSessions(sessions: SessionSummary[]): SessionSummary[] {
  return [...sessions].sort((a, b) => b.last_activity_at - a.last_activity_at);
}

export function reducer(state: State, action: Action): State {
  switch (action.type) {
    case "sessions-loaded":
      return { ...state, sessions: sortSessions(action.sessions) };
    case "session-updated": {
      const rest = state.sessions.filter((s) => s.id !== action.session.id);
      return { ...state, sessions: sortSessions([...rest, action.session]) };
    }
    case "inbox-updated":
      return { ...state, inbox: sortInbox(action.items) };
  }
}
