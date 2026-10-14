import { describe, expect, it } from "vitest";
import {
  TASK_GROUPS,
  canCancel,
  canDiscard,
  canLand,
  canRetryCleanup,
  diffLineClass,
  groupTasks,
  landSummary,
  splitPatch,
} from "./tasks";
import { TASK_STATUSES, type TaskStatus, type TaskSummary } from "./types";

const task = (id: string, status: TaskStatus): TaskSummary => ({
  id,
  project_id: "p1",
  project_name: "proj",
  status,
  title: "t",
  created_at: 1,
  session_id: null,
  branch: "orchestra/t1",
  base_branch: "main",
  note: null,
  last_turn_error: false,
  session_over: false,
});

describe("groupTasks", () => {
  it("places every one of the nine statuses in exactly one group", () => {
    expect(TASK_STATUSES).toHaveLength(9);
    const g = groupTasks(TASK_STATUSES.map((s) => task(s, s)));
    expect(TASK_GROUPS.flatMap((k) => g[k]).map((t) => t.id).sort()).toEqual([...TASK_STATUSES].sort());
    expect(g.queued.map((t) => t.id)).toEqual(["queued"]);
    expect(g.running.map((t) => t.id)).toEqual(["running"]);
    expect(g.needs_input.map((t) => t.id)).toEqual(["needs_input"]);
    expect(g.done.map((t) => t.id)).toEqual(["done", "landing"]);
    expect(g.closed.map((t) => t.id)).toEqual(["landed", "discarded", "cleanup_pending", "error"]);
  });
});

describe("what a task allows", () => {
  it("lands only a done task", () => {
    expect(TASK_STATUSES.filter((s) => canLand(task("x", s)))).toEqual(["done"]);
  });
  it("discards queued, needs_input, done and error, matching Rust", () => {
    expect(TASK_STATUSES.filter((s) => canDiscard(task("x", s)))).toEqual(["queued", "needs_input", "done", "error"]);
  });
  it("cancels only queued and retries only cleanup_pending", () => {
    expect(TASK_STATUSES.filter((s) => canCancel(task("x", s)))).toEqual(["queued"]);
    expect(TASK_STATUSES.filter((s) => canRetryCleanup(task("x", s)))).toEqual(["cleanup_pending"]);
  });
});

describe("diffLineClass", () => {
  it("classes a line by how it starts", () => {
    expect(diffLineClass("+added")).toBe("diff-add");
    expect(diffLineClass("-removed")).toBe("diff-del");
    expect(diffLineClass("@@ -1,2 +1,3 @@")).toBe("diff-hunk");
    expect(diffLineClass("diff --git a/x b/x")).toBe("diff-head");
    expect(diffLineClass("+++ b/x")).toBe("diff-head");
    expect(diffLineClass("--- a/x")).toBe("diff-head");
    expect(diffLineClass(" context")).toBe("diff-ctx");
    expect(diffLineClass("")).toBe("diff-ctx");
  });
});

describe("splitPatch", () => {
  it("keeps a line that holds <script> as a literal string", () => {
    const lines = splitPatch('+<script>alert("x")</script>\n context <img src=x onerror=y>\n');
    expect(lines).toEqual(['+<script>alert("x")</script>', " context <img src=x onerror=y>"]);
  });
  it("handles empty, CRLF and a patch with no trailing newline", () => {
    expect(splitPatch("")).toEqual([]);
    expect(splitPatch("a\r\nb")).toEqual(["a", "b"]);
    expect(splitPatch("a\n\nb\n")).toEqual(["a", "", "b"]);
  });
});

describe("landSummary", () => {
  it("names the project, base branch, file count and method", () => {
    const s = landSummary(task("t1", "done"), 3);
    expect(s).toContain("proj");
    expect(s).toContain("main");
    expect(s).toContain("3 files");
    expect(s).toContain("no fast-forward");
    expect(landSummary(task("t1", "done"), 1)).toContain("1 file,");
  });
});
