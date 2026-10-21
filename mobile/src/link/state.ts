// The phone's view of one host: a pure reducer over decoded host messages. No I/O, no timers.
import type {
  HostMessage,
  InboxItem,
  Project,
  Scope,
  SessionSummary,
  StorageStatus,
  TaskSummary,
} from '../../../protocol';

export type LinkData = {
  hostName: string;
  scopes: string[];
  projects: Project[];
  sessions: SessionSummary[];
  inbox: InboxItem[];
  tasks: TaskSummary[];
  storage: StorageStatus | null;
  // The seq of the last event applied; null until a snapshot has arrived.
  lastSeq: number | null;
};

export const initialData: LinkData = {
  hostName: '',
  scopes: [],
  projects: [],
  sessions: [],
  inbox: [],
  tasks: [],
  storage: null,
  lastSeq: null,
};

export type Reduced = { data: LinkData; gap: boolean };

function upsert<T extends { id: string }>(list: T[], item: T): T[] {
  const at = list.findIndex((x) => x.id === item.id);
  if (at < 0) return [...list, item];
  const next = list.slice();
  next[at] = item;
  return next;
}

export function hasScope(data: LinkData, scope: Scope): boolean {
  return data.scopes.includes(scope);
}

// v1 has no replay: a seq gap means the caller must reconnect for a fresh snapshot.
export function reduce(data: LinkData, msg: HostMessage): Reduced {
  switch (msg.type) {
    case 'welcome':
      return { data: { ...data, hostName: msg.host_name, scopes: msg.scopes }, gap: false };
    case 'snapshot':
      return {
        data: {
          ...data,
          projects: msg.projects,
          sessions: msg.sessions,
          inbox: msg.inbox,
          tasks: msg.tasks,
          storage: msg.storage,
          lastSeq: msg.seq,
        },
        gap: false,
      };
    case 'event':
    case 'unknown_event': {
      // An event with no snapshot to anchor it cannot be ordered; it is dropped.
      if (data.lastSeq === null || msg.seq <= data.lastSeq) return { data, gap: false };
      if (msg.seq > data.lastSeq + 1) return { data, gap: true };
      const next = { ...data, lastSeq: msg.seq };
      if (msg.type === 'event') {
        switch (msg.name) {
          case 'session-updated':
            next.sessions = upsert(data.sessions, msg.payload);
            break;
          case 'task-updated':
            next.tasks = upsert(data.tasks, msg.payload);
            break;
          case 'inbox-updated':
            // One session's items: replace those sessions' items, keep every other session's.
            // An empty payload names no session and changes nothing.
            if (msg.payload.length > 0) {
              const touched = new Set(msg.payload.map((i) => i.session_id));
              next.inbox = [...data.inbox.filter((i) => !touched.has(i.session_id)), ...msg.payload];
            }
            break;
          case 'transcript-appended':
            break;
        }
      }
      return { data: next, gap: false };
    }
    default:
      return { data, gap: false };
  }
}
