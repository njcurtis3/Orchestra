import { useCallback, useEffect, useReducer, useState } from "react";
import {
  listInbox,
  listProjects,
  listSessions,
  listTasks,
  onInboxUpdated,
  onSessionUpdated,
  onTaskUpdated,
  pickProject,
  storageStatus,
  type StorageStatus,
} from "./ipc";
import { initialState, reducer } from "./state";
import type { Project, TaskSummary } from "./types";
import { Inbox } from "./components/Inbox";
import { NewSession } from "./components/NewSession";
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
        if (cancelled) {
          u1();
          u2();
          u3();
          return;
        }
        unlisten.push(u1, u2, u3);
        const [sessions, items, tasks, projs, store] = await Promise.all([
          listSessions(),
          listInbox(),
          listTasks(fail),
          listProjects(),
          storageStatus(),
        ]);
        if (cancelled) return;
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

  return (
    <div className="app">
      <header className="app-header">
        <h1>Orchestra</h1>
        <button type="button" onClick={openProject}>
          Open project
        </button>
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
    </div>
  );
}

export default App;
