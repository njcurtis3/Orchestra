import { useCallback, useEffect, useReducer, useState } from "react";
import {
  linkPending,
  linkStatus,
  listInbox,
  listProjects,
  listSessions,
  listTasks,
  onInboxUpdated,
  onLinkPairingRequest,
  onLinkStatusChanged,
  onSessionUpdated,
  onTaskUpdated,
  pickProject,
  storageStatus,
  type StorageStatus,
} from "./ipc";
import { initialLinkUi, linkReducer, statusPill } from "./link";
import { initialState, reducer } from "./state";
import type { LinkStatus, Project, TaskSummary } from "./types";
import { Devices } from "./components/Devices";
import { Inbox } from "./components/Inbox";
import { NewSession } from "./components/NewSession";
import { PairingDialog } from "./components/PairingDialog";
import { PairingRequest } from "./components/PairingRequest";
import { SessionDetail } from "./components/SessionDetail";
import { SessionList } from "./components/SessionList";
import { TaskDetail } from "./components/TaskDetail";
import { TaskList } from "./components/TaskList";
import "./App.css";

export function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function App() {
  const [state, dispatch] = useReducer(reducer, initialState);
  const [link, linkDispatch] = useReducer(linkReducer, initialLinkUi);
  const [projects, setProjects] = useState<Project[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [storage, setStorage] = useState<StorageStatus>({ ok: true });

  const fail = useCallback((e: unknown) => setError(errText(e)), []);

  useEffect(() => {
    let cancelled = false;
    const unlisten: Array<() => void> = [];
    // Subscribe first so nothing emitted during the initial load is missed.
    void (async () => {
      try {
        const u1 = await onSessionUpdated((session) => dispatch({ type: "session-updated", session }), fail);
        const u2 = await onInboxUpdated((items) => dispatch({ type: "inbox-updated", items }), fail);
        const u3 = await onTaskUpdated((task) => dispatch({ type: "task-updated", task }), fail);
        const u4 = await onLinkStatusChanged((status) => linkDispatch({ type: "status", status }), fail);
        const u5 = await onLinkPairingRequest((request) => linkDispatch({ type: "request", request }), fail);
        if (cancelled) {
          u1();
          u2();
          u3();
          u4();
          u5();
          return;
        }
        unlisten.push(u1, u2, u3, u4, u5);
        const [sessions, items, tasks, projs, store, linkNow, waiting] = await Promise.all([
          listSessions(),
          listInbox(),
          listTasks(fail),
          listProjects(),
          storageStatus(),
          linkStatus(),
          linkPending(),
        ]);
        if (cancelled) return;
        linkDispatch({ type: "status", status: linkNow });
        linkDispatch({ type: "requests-loaded", requests: waiting });
        dispatch({ type: "sessions-loaded", sessions });
        dispatch({ type: "inbox-loaded", items });
        dispatch({ type: "tasks-loaded", tasks });
        setProjects(projs);
        setStorage(store);
      } catch (e) {
        fail(e);
      }
    })();
    return () => {
      cancelled = true;
      unlisten.forEach((u) => u());
    };
  }, [fail]);

  async function openProject() {
    try {
      const p = await pickProject();
      if (p) setProjects(await listProjects());
    } catch (e) {
      fail(e);
    }
  }

  const selectedSession = state.sessions.find((s) => s.id === state.selected);
  const selectedTask = state.tasks.find((t) => t.id === state.selectedTask);
  // A created task is queued; an event may already say more, so an existing copy wins.
  const addTask = useCallback((task: TaskSummary) => dispatch({ type: "tasks-loaded", tasks: [task] }), []);
  const upsertTask = useCallback((task: TaskSummary) => dispatch({ type: "task-updated", task }), []);
  const onLinkStatus = useCallback((status: LinkStatus) => linkDispatch({ type: "status", status }), []);
  const closePairing = useCallback(() => linkDispatch({ type: "pairing", open: false }), []);
  const answered = useCallback((id: string) => linkDispatch({ type: "answered", id }), []);
  const pill = statusPill(link.status);
  const inDevices = link.view === "devices" && link.status !== null;

  return (
    <div className="app">
      <header className="app-header">
        <h1>Orchestra</h1>
        <button type="button" onClick={openProject}>
          Open project
        </button>
        <button
          type="button"
          aria-pressed={inDevices}
          onClick={() => linkDispatch({ type: "show", view: inDevices ? "work" : "devices" })}
        >
          Devices
        </button>
        <span className={`pill pill-${pill.tone}`} role="status">
          {pill.text}
        </span>
        <NewSession projects={projects} onTask={addTask} onError={fail} />
      </header>
      {!storage.ok && (
        <div className="banner" role="alert">
          <span>
            {storage.reason === "unknown_schema_version"
              ? "Saved history was written by a newer version of Orchestra and was not loaded. Nothing will be saved this run."
              : "Saved history could not be read and was not loaded. Nothing will be saved this run."}
          </span>
        </div>
      )}
      {error && (
        <div className="banner" role="alert">
          <span>{error}</span>
          <button type="button" onClick={() => setError(null)}>
            Dismiss
          </button>
        </div>
      )}
      {inDevices && link.status && (
        <main className="columns">
          <Devices
            status={link.status}
            requestWaiting={link.requests.length > 0}
            onStatus={onLinkStatus}
            onPair={() => linkDispatch({ type: "pairing", open: true })}
            onBack={() => linkDispatch({ type: "show", view: "work" })}
            onError={fail}
          />
        </main>
      )}
      {!inDevices && (
        <main className="columns">
          <SessionList
            sessions={state.sessions}
            selected={state.selected}
            onSelect={(id) => dispatch({ type: "select-session", id })}
            onError={fail}
          />
          <TaskList
            tasks={state.tasks}
            selected={state.selectedTask}
            onSelect={(id) => dispatch({ type: "select-task", id })}
            onTask={upsertTask}
            onError={fail}
          />
          {selectedTask ? (
            <TaskDetail
              key={selectedTask.id}
              task={selectedTask}
              hasSession={state.sessions.some((s) => s.id === selectedTask.session_id)}
              onOpenSession={(id) => dispatch({ type: "select-session", id })}
              onTask={upsertTask}
              onBack={() => dispatch({ type: "select-task", id: null })}
            />
          ) : selectedSession ? (
            <SessionDetail
              key={selectedSession.id}
              session={selectedSession}
              onBack={() => dispatch({ type: "select-session", id: null })}
              onError={fail}
            />
          ) : (
            <Inbox items={state.inbox} onError={fail} />
          )}
        </main>
      )}
      {link.pairing && <PairingDialog onClose={closePairing} onError={fail} />}
      {link.requests.length > 0 && (
        <PairingRequest
          key={link.requests[0].id}
          request={link.requests[0]}
          waiting={link.requests.length - 1}
          onDone={answered}
          onError={fail}
        />
      )}
    </div>
  );
}

export default App;
