// A fake WebSocket that plays the host side from the core-generated Noise vectors
// (protocol/fixtures/noise): the reply text, message 2, then frames sealed with the key the
// host sends with. It is a helper, not a suite.
import { fromHex, toHex, utf8Decode, utf8Encode } from '../link/bytes';
import type { WebSocketCtor, WebSocketLike } from '../link/client';
import { open, seal } from '../link/frame';
import { hostId } from '../link/keys';
import { Initiator, prologue } from '../link/noise';
import type { Transport } from '../link/noise';

declare const __dirname: string;
const fs = require('fs') as { readFileSync: (path: string, encoding: 'utf8') => string };
const DIR = `${__dirname}/../../../protocol/fixtures/noise`;

export type Vector = {
  suite: string;
  path: string;
  request_frame: string;
  reply_frame: string;
  client_static_secret_hex: string;
  client_static_public_hex: string;
  client_ephemeral_secret_hex: string;
  host_static_public_hex: string;
  psk_hex?: string;
  msg1_hex: string;
  msg2_hex: string;
  transport: { c2h_hello: { plaintext: string; wire_hex: string } };
};

export function loadVector(name: 'ikpsk2' | 'kk'): Vector {
  return JSON.parse(fs.readFileSync(`${DIR}/${name}.json`, 'utf8')) as Vector;
}

// Static key first (pairing draws a fresh one), then the vector ephemeral key every time.
export function vectorRandom(v: Vector, staticFirst: boolean) {
  let calls = 0;
  return (length: number): Uint8Array => {
    if (length !== 32) throw new Error('unexpected random length');
    calls += 1;
    return fromHex(staticFirst && calls === 1 ? v.client_static_secret_hex : v.client_ephemeral_secret_hex);
  };
}

export const vectorHostId = (v: Vector) => hostId(fromHex(v.host_static_public_hex));

// ---- host messages ----

export const welcomeMsg = (v: Vector, over: Record<string, unknown> = {}) => ({
  v: 1,
  type: 'welcome',
  host_id: vectorHostId(v),
  host_name: 'Vector desktop',
  scopes: ['read', 'answer'],
  features: [],
  ...over,
});

export const sessionOf = (id: string, status = 'working') => ({
  id,
  project_id: 'p1',
  project_name: 'Project',
  status,
  started_at: 1,
  last_activity_at: 2,
  last_message: null,
  claude_session_id: null,
});

export const inboxItem = (id: string) => ({
  id,
  session_id: 's1',
  kind: 'permission',
  created_at: 1,
  resolved: false,
  expired: false,
  summary: 'Allow?',
  tool_name: null,
  tool_input_preview: null,
  questions: null,
  result_text: null,
});

export const snapshotMsg = (seq: number, over: Record<string, unknown> = {}) => ({
  v: 1,
  type: 'snapshot',
  seq,
  projects: [],
  sessions: [sessionOf('s1')],
  inbox: [],
  tasks: [],
  storage: { ok: true },
  ...over,
});

export const eventMsg = (seq: number, name: string, payload: unknown) => ({
  v: 1,
  type: 'event',
  seq,
  name,
  payload,
});

// ---- the fake socket ----

export type Script = {
  // The socket never opens: an error event (the message), then a close (the reason).
  openFail?: { error?: string; reason?: string };
  // The host answers the version request with this text instead of the vector reply.
  reply?: string;
  // The host closes silently after receiving this step.
  silentAfter?: 'request' | 'msg1' | 'hello';
  flipMsg2?: boolean;
  // Sent after the hello opens; defaults to welcome then snapshot.
  live?: unknown[];
  // A TEXT frame sent after the handshake.
  textAfterHello?: string;
  noPong?: boolean;
};

export type FakeHost = {
  WebSocketImpl: WebSocketCtor;
  sockets: FakeSocket[];
};

// Key order is the serialiser's business; the content is what must agree.
const sorted = (x: unknown): string =>
  JSON.stringify(x, (_k, val) =>
    val && typeof val === 'object' && !Array.isArray(val)
      ? Object.fromEntries(Object.entries(val).sort(([a], [b]) => (a < b ? -1 : 1)))
      : val,
  );

const toAB = (b: Uint8Array): ArrayBuffer =>
  b.buffer.slice(b.byteOffset, b.byteOffset + b.byteLength) as ArrayBuffer;

export class FakeSocket implements WebSocketLike {
  binaryType = 'blob';
  onopen: WebSocketLike['onopen'] = null;
  onmessage: WebSocketLike['onmessage'] = null;
  onerror: WebSocketLike['onerror'] = null;
  onclose: WebSocketLike['onclose'] = null;
  closed = false;
  readonly sent: (string | ArrayBuffer)[] = [];
  // What the host read: the msg1 equality, the opened hello and ping count.
  msg1Matches: boolean | null = null;
  hello: unknown = null;
  helloMatchesVector: boolean | null = null;
  pings = 0;
  private host: Transport | null = null;
  private binaries = 0;

  constructor(
    readonly url: string,
    private readonly v: Vector,
    private readonly script: Script,
  ) {
    this.later(() => {
      if (script.openFail) {
        if (script.openFail.error !== undefined) this.onerror?.({ message: script.openFail.error });
        this.closeFromHost(script.openFail.reason ?? '');
      } else this.onopen?.({});
    });
  }

  private later(fn: () => void): void {
    void Promise.resolve().then(() => {
      if (!this.closed) fn();
    });
  }

  closeFromHost(reason = ''): void {
    this.later(() => {
      this.closed = true;
      this.onclose?.({ code: 1006, reason });
    });
  }

  close(): void {
    this.closed = true;
  }

  // Host to client, sealed with the key the host sends with.
  pushHost(message: unknown): void {
    this.later(() => {
      const text = typeof message === 'string' ? message : JSON.stringify(message);
      this.onmessage?.({ data: toAB(seal(this.host!.recv, utf8Encode(text))) });
    });
  }

  pushText(text: string): void {
    this.later(() => this.onmessage?.({ data: text }));
  }

  private pushRaw(bytes: Uint8Array): void {
    this.later(() => this.onmessage?.({ data: toAB(bytes) }));
  }

  send(data: string | ArrayBuffer): void {
    this.sent.push(data);
    const v = this.v;
    if (typeof data === 'string') {
      if (this.script.silentAfter === 'request') return this.closeFromHost();
      return this.later(() => this.onmessage?.({ data: this.script.reply ?? v.reply_frame }));
    }
    this.binaries += 1;
    const bytes = new Uint8Array(data);
    if (this.binaries === 1) {
      this.msg1Matches = toHex(bytes) === v.msg1_hex;
      if (this.script.silentAfter === 'msg1') return this.closeFromHost();
      const msg2 = fromHex(v.msg2_hex);
      if (this.script.flipMsg2) msg2[msg2.length - 1] ^= 1;
      return this.pushRaw(msg2);
    }
    if (!this.host) this.host = this.replay();
    const plain = utf8Decode(open(this.host.send, bytes));
    if (this.binaries === 2) {
      this.hello = JSON.parse(plain);
      this.helloMatchesVector = sorted(this.hello) === sorted(JSON.parse(v.transport.c2h_hello.plaintext));
      if (this.script.silentAfter === 'hello') return this.closeFromHost();
      for (const m of this.script.live ?? [welcomeMsg(v), snapshotMsg(1)]) this.pushHost(m);
      if (this.script.textAfterHello !== undefined) this.pushText(this.script.textAfterHello);
      return;
    }
    if ((JSON.parse(plain) as { type?: string }).type === 'ping') {
      this.pings += 1;
      if (!this.script.noPong) this.pushHost({ v: 1, type: 'pong' });
    }
  }

  // The vector handshake replayed, to learn the transport keys (the host sends with `recv`).
  private replay(): Transport {
    const v = this.v;
    const opts = {
      prologue: prologue(v.path, v.request_frame, v.reply_frame),
      s: {
        secret: fromHex(v.client_static_secret_hex),
        public: fromHex(v.client_static_public_hex),
      },
      rs: fromHex(v.host_static_public_hex),
      random: () => fromHex(v.client_ephemeral_secret_hex),
    };
    const hs =
      v.psk_hex === undefined
        ? Initiator.link(opts)
        : Initiator.pair({ ...opts, psk: fromHex(v.psk_hex) });
    hs.writeMessage1();
    hs.readMessage2(fromHex(v.msg2_hex));
    return hs.split();
  }
}

// Connection n uses scripts[n]; the last script repeats.
export function fakeHost(v: Vector, ...scripts: Script[]): FakeHost {
  const sockets: FakeSocket[] = [];
  const list = scripts.length ? scripts : [{}];
  class Bound extends FakeSocket {
    constructor(url: string) {
      super(url, v, list[Math.min(sockets.length, list.length - 1)]);
      sockets.push(this);
    }
  }
  return { WebSocketImpl: Bound, sockets };
}
