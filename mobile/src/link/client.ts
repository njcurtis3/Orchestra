// One connection to a host, as an explicit phase machine over ws://addr/pair or /link:
//   opening -> version -> msg1-sent -> hello-sent -> live
// It decides nothing about retries. Every failure ends as one LinkFailure, reported once.
// Nothing in here logs, and no failure carries key material or the text of a thrown error.
import {
  decodeHandshakeReply,
  decodeHostMessage,
  encodeClientMessage,
  encodeHandshakeRequest,
} from '../../../protocol/codec';
import type { ClientMessage, HostMessage } from '../../../protocol/types';
import { fromHex, utf8Decode, utf8Encode } from './bytes';
import { open, seal } from './frame';
import { publicKey } from './keys';
import type { Random } from './keys';
import { Initiator, prologue } from './noise';
import type { Transport } from './noise';

export type LinkMode = 'pair' | 'link';
export type LinkPhase = 'opening' | 'version' | 'msg1-sent' | 'hello-sent' | 'live' | 'closed';

export type LinkFailure =
  | { kind: 'Unreachable' }
  | { kind: 'PairWindowClosed' }
  | { kind: 'VersionMismatch'; supported: number[] }
  | { kind: 'LinkRefused' }
  | { kind: 'PairAuthFailed' }
  | { kind: 'PairNotConfirmed' }
  | { kind: 'Revoked' }
  | { kind: 'ProtocolError' }
  | { kind: 'Closed' };

// The slice of the WebSocket API the client uses; React Native's class satisfies it.
export interface WebSocketLike {
  binaryType: string;
  onopen: ((ev: unknown) => void) | null;
  onmessage: ((ev: { data: unknown }) => void) | null;
  onerror: ((ev: { message?: string }) => void) | null;
  onclose: ((ev: { code?: number; reason?: string }) => void) | null;
  send(data: string | ArrayBuffer): void;
  close(): void;
}
export type WebSocketCtor = new (url: string) => WebSocketLike;

export type Timeouts = { openMs: number; handshakeMs: number; pairWelcomeMs: number; linkWelcomeMs: number };
export const TIMEOUTS: Timeouts = {
  openMs: 5_000,
  // The host allows 10 s for the version exchange and the handshake.
  handshakeMs: 10_000,
  // /pair waits for the person at the desktop (the host holds it for 300 s).
  pairWelcomeMs: 310_000,
  linkWelcomeMs: 15_000,
};

export type ConnectOptions = {
  mode: LinkMode;
  addr: string;
  // Lowercase hex: the host static public key, the phone static secret, and (pair) the psk.
  hostKey: string;
  clientSecret: string;
  psk?: string;
  // Checked against welcome.host_id.
  hostId: string;
  clientName: string;
  appVersion: string;
  random: Random;
  WebSocketImpl?: WebSocketCtor;
  // Every decoded host message from welcome on, welcome included.
  onMessage: (msg: HostMessage) => void;
  onPhase?: (phase: LinkPhase) => void;
  // Called exactly once, never before connect() has returned.
  onClose: (failure: LinkFailure) => void;
  timeouts?: Partial<Timeouts>;
};

// Host text frames are read only up to this size.
const MAX_REPLY_BYTES = 1024;
export const MAX_CLIENT_NAME_BYTES = 128;

export type LinkConnection = {
  readonly phase: LinkPhase;
  // The seam for A2 calls. False unless the link is live; nothing here answers a call.
  send(message: ClientMessage): boolean;
  close(): void;
};

// Cut to at most maxBytes of UTF-8 without splitting a code point.
export function clipUtf8(text: string, maxBytes: number): string {
  let out = '';
  let used = 0;
  for (const ch of text) {
    const n = utf8Encode(ch).length;
    if (used + n > maxBytes) break;
    out += ch;
    used += n;
  }
  return out;
}

function toArrayBuffer(bytes: Uint8Array): ArrayBuffer {
  return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;
}

export function connect(o: ConnectOptions): LinkConnection {
  const t: Timeouts = { ...TIMEOUTS, ...o.timeouts };
  const path = o.mode === 'pair' ? '/pair' : '/link';
  let phase: LinkPhase = 'opening';
  let settled = false;
  let ws: WebSocketLike | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let errorText = '';
  let requestText = '';
  // A fresh handshake per connection, dropped after any error and never reused.
  let hs: Initiator | null = null;
  let transport: Transport | null = null;

  const setPhase = (next: LinkPhase) => {
    phase = next;
    o.onPhase?.(next);
  };

  const arm = (ms: number, failure: LinkFailure) => {
    if (timer !== null) clearTimeout(timer);
    timer = setTimeout(() => fail(failure), ms);
  };

  // Report once, drop every reference to key material, and close the socket.
  function fail(failure: LinkFailure): void {
    if (settled) return;
    settled = true;
    if (timer !== null) clearTimeout(timer);
    timer = null;
    hs = null;
    transport = null;
    phase = 'closed';
    const socket = ws;
    if (socket) {
      socket.onopen = socket.onmessage = socket.onerror = socket.onclose = null;
      try {
        socket.close();
      } catch {
        // already closed
      }
    }
    o.onClose(failure);
  }

  // What the socket going away on its own means, by the phase it reached (G6).
  function closedBy(reason: string): LinkFailure {
    switch (phase) {
      case 'opening':
        return o.mode === 'pair' && reason.includes('404')
          ? { kind: 'PairWindowClosed' }
          : { kind: 'Unreachable' };
      case 'version':
        return { kind: 'Unreachable' };
      case 'msg1-sent':
        return o.mode === 'link' ? { kind: 'LinkRefused' } : { kind: 'Unreachable' };
      case 'hello-sent':
        return o.mode === 'pair' ? { kind: 'PairNotConfirmed' } : { kind: 'Closed' };
      default:
        return { kind: 'Closed' };
    }
  }

  function onText(text: string): void {
    if (phase !== 'version') return fail({ kind: 'ProtocolError' });
    if (utf8Encode(text).length > MAX_REPLY_BYTES) return fail({ kind: 'ProtocolError' });
    const reply = decodeHandshakeReply(text);
    if ('type' in reply) {
      if (reply.code === 'unsupported_version') {
        return fail({ kind: 'VersionMismatch', supported: reply.supported ?? [] });
      }
      return fail({ kind: 'ProtocolError' });
    }
    if (!reply.supported.includes(1)) {
      return fail({ kind: 'VersionMismatch', supported: reply.supported });
    }
    const secret = fromHex(o.clientSecret);
    const opts = {
      prologue: prologue(path, requestText, text),
      s: { secret, public: publicKey(secret) },
      rs: fromHex(o.hostKey),
      random: o.random,
    };
    hs =
      o.mode === 'pair'
        ? Initiator.pair({ ...opts, psk: fromHex(o.psk ?? '') })
        : Initiator.link(opts);
    ws!.send(toArrayBuffer(hs.writeMessage1()));
    setPhase('msg1-sent');
  }

  function onBinary(bytes: Uint8Array): void {
    if (phase === 'msg1-sent') {
      const init = hs!;
      try {
        init.readMessage2(bytes);
        transport = init.split();
      } catch {
        // NoiseError or a plain Error from noble: either way the handshake failed.
        return fail({ kind: o.mode === 'pair' ? 'PairAuthFailed' : 'LinkRefused' });
      }
      hs = null;
      const hello = encodeClientMessage({
        v: 1,
        type: 'hello',
        client_name: clipUtf8(o.clientName, MAX_CLIENT_NAME_BYTES),
        app_version: o.appVersion,
      });
      ws!.send(toArrayBuffer(seal(transport.send, utf8Encode(hello))));
      setPhase('hello-sent');
      if (o.mode === 'pair') arm(t.pairWelcomeMs, { kind: 'PairNotConfirmed' });
      else arm(t.linkWelcomeMs, { kind: 'Closed' });
      return;
    }
    if (phase !== 'hello-sent' && phase !== 'live') return fail({ kind: 'ProtocolError' });
    const msg = decodeHostMessage(utf8Decode(open(transport!.recv, bytes)));
    if (msg.type === 'error' && msg.code === 'revoked') return fail({ kind: 'Revoked' });
    if (phase === 'hello-sent') {
      if (msg.type !== 'welcome' || msg.host_id !== o.hostId) return fail({ kind: 'ProtocolError' });
      if (timer !== null) clearTimeout(timer);
      timer = null;
      setPhase('live');
    }
    o.onMessage(msg);
  }

  function start(): void {
    const Ctor = o.WebSocketImpl ?? (globalThis as { WebSocket?: WebSocketCtor }).WebSocket;
    if (!Ctor) throw new Error('no WebSocket');
    const socket = new Ctor(`ws://${o.addr}${path}`);
    ws = socket;
    socket.binaryType = 'arraybuffer';
    arm(t.openMs, { kind: 'Unreachable' });
    socket.onopen = () => {
      if (settled || phase !== 'opening') return;
      try {
        requestText = encodeHandshakeRequest();
        socket.send(requestText);
        setPhase('version');
        arm(t.handshakeMs, { kind: 'Unreachable' });
      } catch {
        fail({ kind: 'ProtocolError' });
      }
    };
    socket.onerror = (ev) => {
      errorText = typeof ev?.message === 'string' ? ev.message : '';
      // Before the socket opens the failure is final; the close that follows is ignored.
      if (phase === 'opening') fail(closedBy(errorText));
    };
    socket.onclose = (ev) => {
      if (settled) return;
      fail(closedBy(`${typeof ev?.reason === 'string' ? ev.reason : ''} ${errorText}`));
    };
    socket.onmessage = (ev) => {
      if (settled) return;
      try {
        const data = ev.data;
        if (typeof data === 'string') onText(data);
        else if (data && typeof (data as ArrayBuffer).byteLength === 'number') {
          onBinary(new Uint8Array(data as ArrayBuffer));
        } else fail({ kind: 'ProtocolError' });
      } catch {
        // Any throw (a decode, a frame, noble) is a failed connection, with no detail kept.
        fail({ kind: 'ProtocolError' });
      }
    };
  }

  try {
    start();
  } catch {
    Promise.resolve().then(() => fail({ kind: 'ProtocolError' }));
  }

  return {
    get phase() {
      return phase;
    },
    send(message) {
      if (settled || phase !== 'live' || !ws || !transport) return false;
      try {
        ws.send(toArrayBuffer(seal(transport.send, utf8Encode(encodeClientMessage(message)))));
        return true;
      } catch {
        fail({ kind: 'ProtocolError' });
        return false;
      }
    },
    close() {
      fail({ kind: 'Closed' });
    },
  };
}
