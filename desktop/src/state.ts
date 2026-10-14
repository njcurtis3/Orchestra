import type { InboxItem, SessionSummary, TaskSummary } from "./types";

export type State = {
  sessions: SessionSummary[];
  inbox: InboxItem[];
  tasks: TaskSummary[];
  selected: string | null; // a session id
  selectedTask: string | null; // a task id; at most one of the two is set
};

export type Action =
  | { type: "sessions-loaded"; sessions: SessionSummary[] }
  | { type: "session-updated"; session: SessionSummary }
  | { type: "inbox-loaded"; items: InboxItem[] }
  | { type: "inbox-updated"; items: InboxItem[] }
  | { type: "select-session"; id: string | null }
  | { type: "tasks-loaded"; tasks: TaskSummary[] }
  | { type: "task-updated"; task: TaskSummary }
  | { type: "select-task"; id: string | null };

export const initialState: State = { sessions: [], inbox: [], tasks: [], selected: null, selectedTask: null };

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

// Oldest first, so the queue reads in the order it will run.
export function sortTasks(tasks: TaskSummary[]): TaskSummary[] {
  return [...tasks].sort((a, b) => a.created_at - b.created_at);
}

// Merge the initial snapshot into what events already delivered. Every Rust state change
// sets last_activity_at = now_ms, so it is monotonic per session: the newer copy wins, and
// on a tie the existing (event) copy wins. A tie is ambiguous only within one millisecond.
function mergeSessions(current: SessionSummary[], loaded: SessionSummary[]): SessionSummary[] {
  const byId = new Map(current.map((s) => [s.id, s]));
  for (const s of loaded) {
    const have = byId.get(s.id);
    if (!have || s.last_activity_at > have.last_activity_at) byId.set(s.id, s);
  }
  return [...byId.values()];
}

// Items are append-only and resolution is monotonic, so a union by id with
// resolved = either side never regresses.
function mergeInbox(current: InboxItem[], loaded: InboxItem[]): InboxItem[] {
  const byId = new Map(current.map((i) => [i.id, i]));
  for (const i of loaded) {
    const have = byId.get(i.id);
    byId.set(i.id, have ? { ...have, resolved: have.resolved || i.resolved } : i);
  }
  return [...byId.values()];
}

export function reducer(state: State, action: Action): State {
  switch (action.type) {
    case "sessions-loaded":
      return { ...state, sessions: sortSessions(mergeSessions(state.sessions, action.sessions)) };
    case "session-updated": {
      const rest = state.sessions.filter((s) => s.id !== action.session.id);
      return { ...state, sessions: sortSessions([...rest, action.session]) };
    }
    case "inbox-loaded":
      return { ...state, inbox: sortInbox(mergeInbox(state.inbox, action.items)) };
    case "inbox-updated": {
      // An event carries one session's items only: upsert them, the event copy wins, and
      // everything else in the inbox stays.
      const byId = new Map(state.inbox.map((i) => [i.id, i]));
      for (const i of action.items) byId.set(i.id, i);
      return { ...state, inbox: sortInbox([...byId.values()]) };
    }
    case "select-session":
      return { ...state, selected: action.id, selectedTask: null };
    case "tasks-loaded": {
      // The subscription came before the snapshot, so a copy already held is at least as
      // new as the snapshot's: it wins, and the snapshot only fills the gaps.
      const byId = new Map(action.tasks.map((t) => [t.id, t]));
      for (const t of state.tasks) byId.set(t.id, t);
      return { ...state, tasks: sortTasks([...byId.values()]) };
    }
    case "task-updated": {
      const rest = state.tasks.filter((t) => t.id !== action.task.id);
      return { ...state, tasks: sortTasks([...rest, action.task]) };
    }
    case "select-task":
      return { ...state, selectedTask: action.id, selected: null };
  }
}
