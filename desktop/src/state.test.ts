import { describe, expect, it } from "vitest";
import { decodeInboxItem, decodeSession } from "./ipc";
import { initialState, reducer } from "./state";
import type { InboxItem, SessionSummary, TaskStatus, TaskSummary } from "./types";

const session = (id: string, at: number, over: Partial<SessionSummary> = {}): SessionSummary => ({
  id,
  project_id: "p1",
  project_name: "proj",
  status: "working",
  started_at: 1,
  last_activity_at: at,
  last_message: null,
  claude_session_id: null,
  ...over,
});

const item = (id: string, at: number, resolved = false): InboxItem => ({
  id,
  session_id: "s1",
  kind: "finished",
  created_at: at,
  resolved,
  expired: false,
  summary: "",
  tool_name: null,
  tool_input_preview: null,
  questions: null,
  result_text: null,
});

describe("reducer", () => {
  it("loads sessions, most recent activity first", () => {
    const s = reducer(initialState, {
      type: "sessions-loaded",
      sessions: [session("a", 1), session("b", 5)],
    });
    expect(s.sessions.map((x) => x.id)).toEqual(["b", "a"]);
  });

  it("inserts a new session on session-updated", () => {
    const s = reducer(initialState, { type: "session-updated", session: session("a", 1) });
    expect(s.sessions).toHaveLength(1);
  });

  it("replaces an existing session by id", () => {
    let s = reducer(initialState, { type: "session-updated", session: session("a", 1) });
    s = reducer(s, { type: "session-updated", session: session("a", 2, { status: "idle" }) });
    expect(s.sessions).toHaveLength(1);
    expect(s.sessions[0].status).toBe("idle");
  });

  it("orders inbox unresolved first, then newest first", () => {
    const s = reducer(initialState, {
      type: "inbox-updated",
      items: [item("old", 1), item("done", 9, true), item("new", 5)],
    });
    expect(s.inbox.map((x) => x.id)).toEqual(["new", "old", "done"]);
  });

  it("inbox-updated upserts and keeps items of other sessions", () => {
    const other = { ...item("x", 2), session_id: "s2" };
    let s = reducer(initialState, { type: "inbox-updated", items: [item("a", 1), other] });
    s = reducer(s, { type: "inbox-updated", items: [item("a", 1, true), item("c", 3)] });
    expect(s.inbox.map((x) => x.id)).toEqual(["c", "x", "a"]);
    expect(s.inbox.find((x) => x.id === "a")?.resolved).toBe(true);
    expect(s.inbox.find((x) => x.id === "x")?.session_id).toBe("s2");
  });

  it("selects a session and goes back", () => {
    let s = reducer(initialState, { type: "select-session", id: "a" });
    expect(s.selected).toBe("a");
    s = reducer(s, { type: "session-updated", session: session("a", 2) });
    expect(s.selected).toBe("a");
    s = reducer(s, { type: "select-session", id: null });
    expect(s.selected).toBeNull();
  });

  it("does not mutate the previous state", () => {
    const before = reducer(initialState, { type: "sessions-loaded", sessions: [session("a", 1)] });
    reducer(before, { type: "session-updated", session: session("b", 2) });
    expect(before.sessions).toHaveLength(1);
  });
});

describe("decoders", () => {
  it("accepts a valid session", () => {
    expect(decodeSession(session("a", 1)).id).toBe("a");
  });

  it("rejects an unknown session status", () => {
    expect(() => decodeSession({ ...session("a", 1), status: "sleeping" })).toThrow();
  });

  it("rejects an unknown inbox kind", () => {
    expect(() => decodeInboxItem({ ...item("a", 1), kind: "alarm" })).toThrow();
  });

  it("rejects a malformed question", () => {
    expect(() =>
      decodeInboxItem({ ...item("a", 1), kind: "question", questions: [{ question: 3 }] }),
    ).toThrow();
  });

  it("sessions-loaded keeps a newer session from an event", () => {
    let s = reducer(initialState, { type: "session-updated", session: session("a", 9, { status: "idle" }) });
    s = reducer(s, { type: "sessions-loaded", sessions: [session("a", 3, { status: "starting" })] });
    expect(s.sessions).toHaveLength(1);
    expect(s.sessions[0].status).toBe("idle");
  });

  it("sessions-loaded adds unseen sessions and keeps ones missing from the snapshot", () => {
    let s = reducer(initialState, { type: "session-updated", session: session("a", 5) });
    s = reducer(s, { type: "sessions-loaded", sessions: [session("b", 2)] });
    expect(s.sessions.map((x) => x.id).sort()).toEqual(["a", "b"]);
  });

  it("inbox-loaded keeps resolved set by an event", () => {
    let s = reducer(initialState, { type: "inbox-updated", items: [item("i1", 1, true)] });
    s = reducer(s, { type: "inbox-loaded", items: [item("i1", 1, false)] });
    expect(s.inbox).toHaveLength(1);
    expect(s.inbox[0].resolved).toBe(true);
  });

  it("inbox-loaded adds unseen items in inbox order", () => {
    let s = reducer(initialState, { type: "inbox-updated", items: [item("i2", 5)] });
    s = reducer(s, { type: "inbox-loaded", items: [item("i1", 1), item("i3", 9)] });
    expect(s.inbox.map((x) => x.id)).toEqual(["i3", "i2", "i1"]);
  });
});

const task = (id: string, at: number, status: TaskStatus = "queued"): TaskSummary => ({
  id,
  project_id: "p1",
  project_name: "proj",
  status,
  title: "t",
  created_at: at,
  session_id: null,
  branch: null,
  base_branch: "main",
  note: null,
  last_turn_error: false,
  session_over: false,
});

describe("reducer tasks", () => {
  it("inserts on task-updated and replaces by id, oldest first", () => {
    let s = reducer(initialState, { type: "task-updated", task: task("b", 2) });
    s = reducer(s, { type: "task-updated", task: task("a", 1) });
    expect(s.tasks.map((t) => t.id)).toEqual(["a", "b"]);
    s = reducer(s, { type: "task-updated", task: task("a", 1, "running") });
    expect(s.tasks).toHaveLength(2);
    expect(s.tasks[0].status).toBe("running");
  });

  it("merges a snapshot under what events already delivered", () => {
    let s = reducer(initialState, { type: "task-updated", task: task("a", 1, "done") });
    s = reducer(s, { type: "tasks-loaded", tasks: [task("a", 1, "running"), task("b", 2)] });
    expect(s.tasks.map((t) => [t.id, t.status])).toEqual([
      ["a", "done"],
      ["b", "queued"],
    ]);
  });

  it("selecting a task clears the selected session and the reverse", () => {
    let s = reducer(initialState, { type: "select-session", id: "s1" });
    s = reducer(s, { type: "select-task", id: "t1" });
    expect([s.selected, s.selectedTask]).toEqual([null, "t1"]);
    s = reducer(s, { type: "select-session", id: "s2" });
    expect([s.selected, s.selectedTask]).toEqual(["s2", null]);
  });
});
