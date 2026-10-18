import { describe, expect, it } from "vitest";
import {
  decodeDevice,
  decodeInboxItem,
  decodeLinkInterface,
  decodeLinkStatus,
  decodePairingView,
  decodePendingPairing,
  decodeSession,
  decodeStorageStatus,
  decodeTask,
  decodeTaskDiff,
  decodeTasks,
  decodeTranscript,
  decodeTranscriptBatch,
  decodeTranscriptRecord,
} from "./ipc";

const wire = (status: unknown) => ({
  id: "s1",
  project_id: "p1",
  project_name: "proj",
  status,
  started_at: 1,
  last_activity_at: 2,
  last_message: null,
  claude_session_id: null,
});

describe("decodeSession settings", () => {
  it("decodes a listed model and mode, and null or absent as null", () => {
    const s = decodeSession({ ...wire("working"), model: "fable", permission_mode: "acceptEdits" });
    expect([s.model, s.permission_mode]).toEqual(["fable", "acceptEdits"]);
    expect(decodeSession({ ...wire("working"), model: null }).model).toBeNull();
    expect(decodeSession(wire("working")).permission_mode).toBeNull();
  });

  it("rejects a model or mode outside the lists", () => {
    expect(() => decodeSession({ ...wire("working"), model: "haiku" })).toThrow("unknown model");
    expect(() => decodeSession({ ...wire("working"), permission_mode: "bypassPermissions" })).toThrow(
      "unknown permission_mode",
    );
  });
});

describe("decodeSession status", () => {
  it("accepts needs_attention", () => {
    expect(decodeSession(wire("needs_attention")).status).toBe("needs_attention");
  });

  it("rejects an unknown status", () => {
    expect(() => decodeSession(wire("on_fire"))).toThrow("unknown status");
  });
});

const itemWire = (over: Record<string, unknown> = {}) => ({
  id: "s1-i1",
  session_id: "s1",
  kind: "finished",
  created_at: 1,
  resolved: false,
  expired: false,
  summary: "done",
  tool_name: null,
  tool_input_preview: null,
  questions: null,
  result_text: null,
  ...over,
});

describe("decodeSession suspended", () => {
  it("accepts suspended", () => {
    expect(decodeSession(wire("suspended")).status).toBe("suspended");
  });
});

describe("decodeInboxItem", () => {
  it("decodes expired and result_text", () => {
    const i = decodeInboxItem(itemWire({ resolved: true, expired: true, result_text: "full text" }));
    expect([i.expired, i.result_text]).toEqual([true, "full text"]);
  });

  it("treats a missing result_text as null", () => {
    const { result_text: _drop, ...rest } = itemWire();
    expect(decodeInboxItem(rest).result_text).toBeNull();
  });

  it("rejects an item missing expired", () => {
    const { expired: _drop, ...rest } = itemWire();
    expect(() => decodeInboxItem(rest)).toThrow("bad field expired");
  });

  it("rejects a mistyped expired or result_text", () => {
    expect(() => decodeInboxItem(itemWire({ expired: "yes" }))).toThrow("bad field expired");
    expect(() => decodeInboxItem(itemWire({ result_text: 3 }))).toThrow("bad field result_text");
  });
});

describe("decodeStorageStatus", () => {
  it("decodes ok and the two failure reasons", () => {
    expect(decodeStorageStatus({ ok: true })).toEqual({ ok: true });
    expect(decodeStorageStatus({ ok: false, reason: "unknown_schema_version", found_version: 2 })).toEqual({
      ok: false,
      reason: "unknown_schema_version",
      found_version: 2,
    });
    expect(decodeStorageStatus({ ok: false, reason: "unreadable" })).toEqual({
      ok: false,
      reason: "unreadable",
      found_version: null,
    });
  });

  it("rejects an unknown reason", () => {
    expect(() => decodeStorageStatus({ ok: false, reason: "disk_on_fire" })).toThrow("unknown reason");
  });
});

describe("decodeTranscript", () => {
  const rec = (over: Record<string, unknown>) => ({ seq: 1, at: 5, ...over });
  const good = [
    rec({ kind: "user_prompt", text: "hi", truncated: false }),
    rec({ seq: 2, kind: "tool_use", id: "t", name: "Bash", input_preview: "{}", truncated: false }),
    rec({ seq: 3, kind: "tool_result", tool_use_id: "t", is_error: false, content: "ok", truncated: true }),
    rec({
      seq: 4,
      kind: "turn_result",
      is_error: false,
      subtype: "success",
      total_cost_usd: 0.5,
      duration_ms: null,
      num_turns: 2,
      usage: { input: 1, output: 2, cache_read: 3, cache_creation: 4 },
    }),
    rec({ seq: 5, kind: "truncated" }),
  ];

  it("decodes every entry kind", () => {
    const page = decodeTranscript({ entries: good, next_seq: 6 });
    expect(page.entries.map((e) => e.kind)).toEqual(["user_prompt", "tool_use", "tool_result", "turn_result", "truncated"]);
    expect(page.next_seq).toBe(6);
  });

  it("drops malformed entries and keeps the rest", () => {
    const bad = [
      rec({ seq: 7, kind: "alarm" }),
      rec({ seq: 8, kind: "assistant_text", text: 3, truncated: false }),
      rec({ seq: 9, kind: "tool_result", tool_use_id: "t", is_error: "no", content: "", truncated: false }),
      { kind: "user_prompt", text: "no seq", truncated: false },
      "junk",
      null,
    ];
    const page = decodeTranscript({ entries: [good[0], ...bad, good[4]], next_seq: 10 });
    expect(page.entries.map((e) => e.seq)).toEqual([1, 5]);
  });

  it("rejects a page or batch whose envelope is wrong", () => {
    expect(() => decodeTranscript({ entries: good })).toThrow("bad field next_seq");
    expect(() => decodeTranscript({ entries: "x", next_seq: 1 })).toThrow();
    expect(() => decodeTranscriptBatch({ entries: [] })).toThrow("bad field session_id");
    expect(decodeTranscriptBatch({ session_id: "s1", entries: [good[0], { kind: "x" }] }).entries).toHaveLength(1);
  });

  it("throws on a single bad record", () => {
    expect(() => decodeTranscriptRecord(rec({ kind: "tool_use", id: "t" }))).toThrow();
  });
});

const taskWire = (status: unknown, extra: Record<string, unknown> = {}) => ({
  id: "t1",
  project_id: "p1",
  project_name: "proj",
  status,
  title: "do it",
  created_at: 1,
  session_id: null,
  branch: "orchestra/t1",
  base_branch: "main",
  note: null,
  last_turn_error: false,
  session_over: false,
  ...extra,
});

describe("decodeTask", () => {
  it("decodes every status Rust can send", () => {
    for (const s of ["queued", "running", "needs_input", "done", "landing", "landed", "discarded", "cleanup_pending", "error"]) {
      expect(decodeTask(taskWire(s)).status).toBe(s);
    }
  });

  it("drops an unknown status through onDrop and keeps the rest of the list", () => {
    expect(() => decodeTask(taskWire("paused"))).toThrow("unknown status");
    const dropped: unknown[] = [];
    const list = decodeTasks([taskWire("done"), taskWire("paused", { id: "t2" })], (e) => dropped.push(e));
    expect(list.map((t) => t.id)).toEqual(["t1"]);
    expect(dropped).toHaveLength(1);
  });

  it("ignores an extra path-like field", () => {
    const t = decodeTask(taskWire("done", { worktree: "C:/data/worktrees/p1/t1", path: "/tmp/zzz" }));
    expect(Object.keys(t)).not.toContain("worktree");
    expect(Object.keys(t)).not.toContain("path");
    expect(JSON.stringify(t)).not.toContain("zzz");
    expect(JSON.stringify(t)).not.toContain("worktrees");
  });

  it("rejects a missing required field", () => {
    expect(() => decodeTask({ ...taskWire("done"), base_branch: undefined })).toThrow("bad field base_branch");
  });
});

describe("decodeTaskDiff", () => {
  const wireDiff = {
    files: [{ path: "a.txt", status: "added", added: 2, removed: 0, binary: false }],
    files_truncated: false,
    patch: "+x\n",
    truncated: true,
    commits: [{ id: "abc1234", subject: "s", body: "b\n\nCo-Authored-By: x" }],
    base_branch: "main",
  };
  it("decodes files, commits with their full body, and the truncation flags", () => {
    const d = decodeTaskDiff(wireDiff);
    expect(d.commits[0].body).toContain("Co-Authored-By");
    expect(d.truncated).toBe(true);
    expect(d.files[0].added).toBe(2);
  });
  it("rejects an unknown file status", () => {
    expect(() => decodeTaskDiff({ ...wireDiff, files: [{ ...wireDiff.files[0], status: "renamed" }] })).toThrow();
  });
});

const linkWire = () => ({
  available: true,
  os: "windows",
  enabled: true,
  running: true,
  addrs: ["127.0.0.1:47800"],
  port: 47800,
  binds: ["127.0.0.1"],
  missing: [],
  error: null,
  config_readonly: null,
  firewall_notice_seen: false,
});

describe("decodeLinkStatus", () => {
  it("decodes a status and keeps null as null", () => {
    const s = decodeLinkStatus(linkWire());
    expect([s.os, s.running, s.addrs, s.error, s.config_readonly]).toEqual([
      "windows",
      true,
      ["127.0.0.1:47800"],
      null,
      null,
    ]);
    expect(decodeLinkStatus({ ...linkWire(), error: "Port 47800 is already in use." }).error).toContain("47800");
  });

  it("rejects an unknown OS, a missing field and a non-string address", () => {
    expect(() => decodeLinkStatus({ ...linkWire(), os: "plan9" })).toThrow("unknown os");
    const { running: _running, ...rest } = linkWire();
    expect(() => decodeLinkStatus(rest)).toThrow("bad field running");
    expect(() => decodeLinkStatus({ ...linkWire(), addrs: [1] })).toThrow("bad field addrs");
  });
});

describe("decodeLinkInterface", () => {
  it("decodes the four kinds and rejects another", () => {
    for (const kind of ["loopback", "lan", "tailscale", "other"]) {
      expect(decodeLinkInterface({ ip: "10.0.0.1", kind, name: "x" }).kind).toBe(kind);
    }
    expect(() => decodeLinkInterface({ ip: "10.0.0.1", kind: "vpn", name: "x" })).toThrow("unknown kind");
  });
});

describe("decodePairingView", () => {
  const view = {
    qr_svg: '<?xml version="1.0" standalone="yes"?><svg xmlns="http://www.w3.org/2000/svg"></svg>',
    host_name: "Desk",
    addrs: ["127.0.0.1:47800"],
    expires_at: 5,
  };

  it("decodes the view", () => {
    expect(decodePairingView(view).host_name).toBe("Desk");
  });

  it("rejects a payload that carries a secret field, however it is spelled", () => {
    for (const key of ["secret", "psk", "pairing_secret", "private_key"]) {
      expect(() => decodePairingView({ ...view, [key]: "ab".repeat(32) }), key).toThrow("bad pairing");
    }
  });

  it("rejects an image that is not an svg", () => {
    expect(() => decodePairingView({ ...view, qr_svg: "<script>alert(1)</script>" })).toThrow("bad field qr_svg");
    expect(() => decodePairingView({ ...view, qr_svg: "data:image/png;base64,AAAA" })).toThrow("bad field qr_svg");
  });
});

describe("decodePendingPairing and decodeDevice", () => {
  it("decodes a request", () => {
    const p = decodePendingPairing({ id: "p1", client_name: "Pixel", fingerprint: "aaaa-bbbb", expires_at: 9 });
    expect([p.id, p.client_name, p.expires_at]).toEqual(["p1", "Pixel", 9]);
    expect(() => decodePendingPairing({ id: "p1", client_name: "Pixel", expires_at: 9 })).toThrow(
      "bad field fingerprint",
    );
  });

  it("decodes a device and refuses a scope outside the four", () => {
    const d = decodeDevice({ id: "c-1", name: "Pixel", fingerprint: "aaaa-bbbb", scopes: ["read", "land"], paired_at: 3 });
    expect(d.scopes).toEqual(["read", "land"]);
    expect(() => decodeDevice({ id: "c-1", name: "Pixel", fingerprint: "f", scopes: ["root"], paired_at: 3 })).toThrow(
      "unknown scope",
    );
  });
});
