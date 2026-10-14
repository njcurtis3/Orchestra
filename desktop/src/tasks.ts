import type { TaskStatus, TaskSummary } from "./types";

// Pure helpers for the Tasks column and the review. No I/O.

export const TASK_STATUS_LABELS: Record<TaskStatus, string> = {
  queued: "Queued",
  running: "Running",
  needs_input: "Needs input",
  done: "Done",
  landing: "Landing",
  landed: "Landed",
  discarded: "Discarded",
  cleanup_pending: "Cleanup pending",
  error: "Error",
};

export const TASK_GROUPS = ["queued", "running", "needs_input", "done", "closed"] as const;
export type TaskGroup = (typeof TASK_GROUPS)[number];

export const TASK_GROUP_LABELS: Record<TaskGroup, string> = {
  queued: "Queued",
  running: "Running",
  needs_input: "Needs input",
  done: "Done",
  closed: "Landed, discarded, error, cleanup pending",
};

// Record, not a list: tsc fails if a new status is added without choosing its group.
// Landing sits with done, where the task was a moment ago.
const GROUP_OF: Record<TaskStatus, TaskGroup> = {
  queued: "queued",
  running: "running",
  needs_input: "needs_input",
  done: "done",
  landing: "done",
  landed: "closed",
  discarded: "closed",
  cleanup_pending: "closed",
  error: "closed",
};

export function groupTasks(tasks: TaskSummary[]): Record<TaskGroup, TaskSummary[]> {
  const out: Record<TaskGroup, TaskSummary[]> = { queued: [], running: [], needs_input: [], done: [], closed: [] };
  for (const t of tasks) out[GROUP_OF[t.status]].push(t);
  return out;
}

// Rust enforces all of these; they only decide whether a button shows.
export const canLand = (t: Pick<TaskSummary, "status">): boolean => t.status === "done";
export const canDiscard = (t: Pick<TaskSummary, "status">): boolean =>
  t.status === "queued" || t.status === "needs_input" || t.status === "done" || t.status === "error";
export const canCancel = (t: Pick<TaskSummary, "status">): boolean => t.status === "queued";
export const canRetryCleanup = (t: Pick<TaskSummary, "status">): boolean => t.status === "cleanup_pending";

// The class comes from the start of the line only; the text is never interpreted.
export function diffLineClass(line: string): string {
  if (line.startsWith("@@")) return "diff-hunk";
  if (
    line.startsWith("diff ") ||
    line.startsWith("index ") ||
    line.startsWith("+++") ||
    line.startsWith("---") ||
    line.startsWith("new file") ||
    line.startsWith("deleted file") ||
    line.startsWith("similarity ") ||
    line.startsWith("rename ") ||
    line.startsWith("Binary files")
  ) {
    return "diff-head";
  }
  if (line.startsWith("+")) return "diff-add";
  if (line.startsWith("-")) return "diff-del";
  return "diff-ctx";
}

// Lines as literal strings; a trailing newline does not make an empty last line.
export function splitPatch(patch: string): string[] {
  if (patch === "") return [];
  const lines = patch.split(/\r?\n/);
  if (lines[lines.length - 1] === "") lines.pop();
  return lines;
}

// What the land confirm names, so the owner sees what will happen before it does.
export function landSummary(t: TaskSummary, fileCount: number): string {
  const files = `${fileCount} file${fileCount === 1 ? "" : "s"}`;
  return `Merge ${t.branch ?? "the task branch"} into ${t.base_branch} of ${t.project_name} (${files}, merge commit, no fast-forward).`;
}
