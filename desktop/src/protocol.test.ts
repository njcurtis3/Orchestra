// The canonical run of the shared protocol fixtures (protocol/fixtures). The Rust copy runs
// the same files in core/tests/wire_fixtures.rs; mobile runs them under jest.
import { describe, expect, it } from "vitest";
import {
  ERROR_CODES,
  EVENT_NAMES,
  MAX_ID_BYTES,
  MAX_MESSAGE_BYTES,
  MAX_TEXT_BYTES,
  METHODS,
  METHOD_SCOPE,
  PROTOCOL_VERSION,
  ProtocolError,
  SCOPES,
  SUPPORTED_VERSIONS,
  decodeClientMessage,
  decodeHandshakeReply,
  decodeHandshakeRequest,
  decodeHostMessage,
  encodeClientMessage,
  encodeHandshakeReply,
  encodeHandshakeRequest,
  encodeHostMessage,
  utf8Length,
  type Answer as WireAnswer,
  type InboxItem as WireInboxItem,
  type SessionSummary as WireSession,
  type TaskSummary as WireTask,
  type TranscriptBatch as WireBatch,
} from "../../protocol";
import { FIXTURES, constants, type FixtureCase } from "../../protocol/fixtures";
import {
  MAX_TEXT_BYTES as DESKTOP_MAX_TEXT_BYTES,
  type Answer,
  type InboxItem,
  type SessionSummary,
  type TaskSummary,
  type TranscriptBatch,
} from "./types";

// Compile-time lock: the desktop IPC types are assignable to the wire types, so the local
// copy cannot drift away from the protocol without `npm run build` failing.
type Assignable<A extends B, B> = [A, B];
export type DesktopMatchesWire = [
  Assignable<SessionSummary, WireSession>,
  Assignable<InboxItem, WireInboxItem>,
  Assignable<TaskSummary, WireTask>,
  Assignable<TranscriptBatch, WireBatch>,
  Assignable<Answer, WireAnswer>,
];

type Codec = { decode: (text: string) => unknown; encode: (v: never) => string };
const CODECS: Record<FixtureCase["dir"], Codec> = {
  c2h: { decode: decodeClientMessage, encode: encodeClientMessage },
  h2c: { decode: decodeHostMessage, encode: encodeHostMessage },
  hs_c2h: { decode: decodeHandshakeRequest, encode: encodeHandshakeRequest },
  hs_h2c: { decode: decodeHandshakeReply, encode: encodeHandshakeReply },
};

function errorOf(fn: () => unknown): unknown {
  try {
    fn();
  } catch (e) {
    return e;
  }
  return undefined;
}

describe("fixtures", () => {
  for (const c of FIXTURES) {
    it(c.name, () => {
      const text = c.raw ?? JSON.stringify(c.wire);
      const { decode, encode } = CODECS[c.dir];
      if (c.expect === "ok") {
        const decoded = decode(text);
        expect(JSON.parse(encode(decoded as never))).toEqual(c.decoded_wire ?? c.wire);
      } else {
        const err = errorOf(() => decode(text));
        expect(err).toBeInstanceOf(ProtocolError);
        expect((err as ProtocolError).code).toBe(c.expect.error);
      }
    });
  }

  it("index.ts covers every fixtures/*.json", () => {
    const files = Object.keys(import.meta.glob("../../protocol/fixtures/*.json"))
      .map((p) => p.slice(p.lastIndexOf("/") + 1, -".json".length))
      .sort();
    const listed = [...FIXTURES.map((c) => c.name), "constants"].sort();
    expect(listed).toEqual(files);
  });

  it("covers every method, every answer kind, every event and every type", () => {
    const ok = FIXTURES.filter((c) => c.expect === "ok");
    const wires = (dir: string) => ok.filter((c) => c.dir === dir).map((c) => c.wire as Record<string, unknown>);
    const calls = wires("c2h").filter((w) => w.type === "call");
    expect([...new Set(calls.map((w) => w.method))].sort()).toEqual([...METHODS].sort());
    const kinds = calls.filter((w) => w.method === "answer").map((w) => (w.args as { kind: string }).kind);
    expect([...new Set(kinds)].sort()).toEqual(["permission", "question", "reply"]);
    expect([...new Set(wires("c2h").map((w) => w.type))].sort()).toEqual(["call", "hello", "ping"]);
    const host = wires("h2c");
    expect([...new Set(host.map((w) => w.type))].sort()).toEqual(
      ["error", "event", "pong", "result", "snapshot", "welcome"],
    );
    const events = host.filter((w) => w.type === "event").map((w) => w.name);
    for (const name of EVENT_NAMES.filter((n) => n !== "notice")) expect(events).toContain(name);
  });
});

describe("constants", () => {
  it("match fixtures/constants.json", () => {
    expect(constants).toEqual({
      v: PROTOCOL_VERSION,
      supported: [...SUPPORTED_VERSIONS],
      max_text_bytes: MAX_TEXT_BYTES,
      max_message_bytes: MAX_MESSAGE_BYTES,
      max_id_bytes: MAX_ID_BYTES,
      methods: [...METHODS],
      method_scope: METHOD_SCOPE,
      event_names: [...EVENT_NAMES],
      scopes: [...SCOPES],
      error_codes: [...ERROR_CODES],
    });
  });

  it("the desktop copy of MAX_TEXT_BYTES is the protocol's", () => {
    expect(DESKTOP_MAX_TEXT_BYTES).toBe(MAX_TEXT_BYTES);
  });
});

describe("limits", () => {
  // A pong carries no required field, so padding it to an exact size isolates the size check.
  const padded = (type: string, size: number): string => {
    const base = JSON.stringify({ v: 1, type, pad: "" });
    return JSON.stringify({ v: 1, type, pad: "x".repeat(size - utf8Length(base)) });
  };

  it("a host message of exactly MAX_MESSAGE_BYTES decodes and one byte more does not", () => {
    expect(decodeHostMessage(padded("pong", MAX_MESSAGE_BYTES))).toEqual({ v: 1, type: "pong" });
    const err = errorOf(() => decodeHostMessage(padded("pong", MAX_MESSAGE_BYTES + 1)));
    expect((err as ProtocolError).code).toBe("bad_request");
    expect((err as ProtocolError).message).toMatch(/too large/);
  });

  it("a client message over MAX_MESSAGE_BYTES is too large, checked before parsing", () => {
    const err = errorOf(() => decodeClientMessage(padded("ping", MAX_MESSAGE_BYTES + 1)));
    expect((err as ProtocolError).code).toBe("bad_request");
    expect((err as ProtocolError).message).toMatch(/too large/);
  });

  it("a client string is capped at MAX_TEXT_BYTES in UTF-8 bytes", () => {
    const hello = (name: string) => JSON.stringify({ v: 1, type: "hello", client_name: name, app_version: "1" });
    expect(decodeClientMessage(hello("x".repeat(MAX_TEXT_BYTES)))).toMatchObject({ type: "hello" });
    const err = errorOf(() => decodeClientMessage(hello("x".repeat(MAX_TEXT_BYTES + 1))));
    expect((err as ProtocolError).code).toBe("bad_request");
    const wide = errorOf(() => decodeClientMessage(hello("é".repeat(MAX_TEXT_BYTES / 2 + 1))));
    expect((wide as ProtocolError).message).toMatch(/too long/);
  });

  it("utf8Length agrees with TextEncoder", () => {
    for (const s of ["", "abc", "é", "€", "\u{1f600}", "a\u{1f600}é€z"]) {
      expect(utf8Length(s)).toBe(new TextEncoder().encode(s).length);
    }
  });
});
