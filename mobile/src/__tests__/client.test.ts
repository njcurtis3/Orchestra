import { clipUtf8, connect } from '../link/client';
import type { ConnectOptions, LinkFailure } from '../link/client';
import type { HostMessage } from '../../../protocol/types';
import { fakeHost, loadVector, snapshotMsg, vectorHostId, vectorRandom, welcomeMsg } from './fake-host';
import type { Script, Vector } from './fake-host';

const flush = async () => {
  for (let i = 0; i < 20; i++) await Promise.resolve();
};

function run(v: Vector, mode: 'pair' | 'link', script: Script, over: Partial<ConnectOptions> = {}) {
  const host = fakeHost(v, script);
  const messages: HostMessage[] = [];
  const phases: string[] = [];
  const closes: LinkFailure[] = [];
  const conn = connect({
    mode,
    addr: '192.168.1.20:47800',
    hostKey: v.host_static_public_hex,
    clientSecret: v.client_static_secret_hex,
    psk: v.psk_hex,
    hostId: vectorHostId(v),
    clientName: 'Vector phone',
    appVersion: '1.0.0',
    random: vectorRandom(v, false),
    WebSocketImpl: host.WebSocketImpl,
    onMessage: (m) => messages.push(m),
    onPhase: (p) => phases.push(p),
    onClose: (f) => closes.push(f),
    ...over,
  });
  return { host, conn, messages, phases, closes, sock: () => host.sockets[0] };
}

const isArrayBuffer = (x: unknown) => Object.prototype.toString.call(x) === '[object ArrayBuffer]';
const HTTP_404 = "Expected HTTP 101 response but was '404 Not Found'";

describe.each([
  ['pair', 'ikpsk2'],
  ['link', 'kk'],
] as const)('%s happy path', (mode, name) => {
  const v = loadVector(name);

  it('reaches welcome and snapshot, and the hello opens with the vector key', async () => {
    const r = run(v, mode, {});
    await flush();
    expect(r.closes).toEqual([]);
    expect(r.phases).toEqual(['version', 'msg1-sent', 'hello-sent', 'live']);
    expect(r.messages.map((m) => m.type)).toEqual(['welcome', 'snapshot']);
    expect(r.sock().url).toBe(`ws://192.168.1.20:47800${v.path}`);
    expect(r.sock().binaryType).toBe('arraybuffer');
    expect(r.sock().msg1Matches).toBe(true);
    expect(r.sock().hello).toEqual({
      v: 1,
      type: 'hello',
      client_name: 'Vector phone',
      app_version: '1.0.0',
    });
    expect(r.sock().helloMatchesVector).toBe(true);
    expect(r.sock().sent[0]).toBe(v.request_frame);
    expect(r.conn.phase).toBe('live');
  });

  it('sends every frame after the version request as an ArrayBuffer', async () => {
    const r = run(v, mode, {});
    await flush();
    expect(r.conn.send({ v: 1, type: 'ping' })).toBe(true);
    await flush();
    const sent = r.sock().sent;
    expect(sent.length).toBe(4);
    expect(typeof sent[0]).toBe('string');
    expect(sent.slice(1).every(isArrayBuffer)).toBe(true);
    expect(r.sock().pings).toBe(1);
    expect(r.messages.map((m) => m.type)).toEqual(['welcome', 'snapshot', 'pong']);
  });

  it('does not send before live', async () => {
    const r = run(v, mode, { live: [] });
    await flush();
    expect(r.conn.send({ v: 1, type: 'ping' })).toBe(false);
  });

  it('clips client_name to 128 bytes', async () => {
    const r = run(v, mode, {}, { clientName: 'é'.repeat(100) });
    await flush();
    const name = (r.sock().hello as { client_name: string }).client_name;
    expect(new TextEncoder().encode(name).length).toBe(128);
  });

  it('reports a version error with the supported list', async () => {
    const reply = JSON.stringify({
      type: 'error',
      v: 1,
      code: 'unsupported_version',
      message: 'no',
      supported: [1],
    });
    const r = run(v, mode, { reply });
    await flush();
    expect(r.closes).toEqual([{ kind: 'VersionMismatch', supported: [1] }]);
    expect(r.sock().sent.length).toBe(1);
  });

  it('closes with ProtocolError on a TEXT frame after the handshake', async () => {
    const r = run(v, mode, { textAfterHello: '{"v":1,"type":"pong"}' });
    await flush();
    expect(r.closes).toEqual([{ kind: 'ProtocolError' }]);
    expect(r.sock().closed).toBe(true);
  });

  it('closes with ProtocolError on a welcome for another host', async () => {
    const r = run(v, mode, { live: [welcomeMsg(v, { host_id: 'ab'.repeat(16) })] });
    await flush();
    expect(r.closes).toEqual([{ kind: 'ProtocolError' }]);
  });

  it('closes with ProtocolError when the first message is not welcome', async () => {
    const r = run(v, mode, { live: [snapshotMsg(1)] });
    await flush();
    expect(r.closes).toEqual([{ kind: 'ProtocolError' }]);
  });

  it('closes with ProtocolError on an undecodable message', async () => {
    const r = run(v, mode, { live: [welcomeMsg(v), 'not json'] });
    await flush();
    expect(r.closes).toEqual([{ kind: 'ProtocolError' }]);
  });

  it('maps error revoked to Revoked', async () => {
    const r = run(v, mode, {
      live: [{ v: 1, type: 'error', code: 'revoked', message: 'revoked' }],
    });
    await flush();
    expect(r.closes).toEqual([{ kind: 'Revoked' }]);
  });

  it('reports once', async () => {
    const r = run(v, mode, {});
    await flush();
    r.conn.close();
    r.conn.close();
    expect(r.closes).toEqual([{ kind: 'Closed' }]);
    expect(r.conn.phase).toBe('closed');
  });

  it('reports Unreachable when the socket never opens', async () => {
    const r = run(v, mode, { openFail: { error: 'connect failed', reason: 'connect failed' } });
    await flush();
    expect(r.closes).toEqual([{ kind: 'Unreachable' }]);
  });
});

describe('link only', () => {
  const v = loadVector('kk');

  it('close after msg1 gives LinkRefused', async () => {
    const r = run(v, 'link', { silentAfter: 'msg1' });
    await flush();
    expect(r.closes).toEqual([{ kind: 'LinkRefused' }]);
  });

  it('a flipped msg2 bit gives LinkRefused', async () => {
    const r = run(v, 'link', { flipMsg2: true });
    await flush();
    expect(r.closes).toEqual([{ kind: 'LinkRefused' }]);
  });

  it('a 404 is just Unreachable on /link', async () => {
    const r = run(v, 'link', { openFail: { reason: HTTP_404 } });
    await flush();
    expect(r.closes).toEqual([{ kind: 'Unreachable' }]);
  });
});

describe('pair only', () => {
  const v = loadVector('ikpsk2');

  it('a flipped msg2 bit gives PairAuthFailed', async () => {
    const r = run(v, 'pair', { flipMsg2: true });
    await flush();
    expect(r.closes).toEqual([{ kind: 'PairAuthFailed' }]);
    expect(r.sock().sent.length).toBe(2);
  });

  it('close after hello gives PairNotConfirmed', async () => {
    const r = run(v, 'pair', { silentAfter: 'hello' });
    await flush();
    expect(r.closes).toEqual([{ kind: 'PairNotConfirmed' }]);
  });

  it('a 404 in the close reason gives PairWindowClosed', async () => {
    const r = run(v, 'pair', { openFail: { reason: HTTP_404 } });
    await flush();
    expect(r.closes).toEqual([{ kind: 'PairWindowClosed' }]);
  });

  it('a 404 in the error message gives PairWindowClosed', async () => {
    const r = run(v, 'pair', { openFail: { error: HTTP_404 } });
    await flush();
    expect(r.closes).toEqual([{ kind: 'PairWindowClosed' }]);
  });

  it('a wrong psk gives PairAuthFailed', async () => {
    const r = run(v, 'pair', {}, { psk: '00'.repeat(32) });
    await flush();
    expect(r.closes).toEqual([{ kind: 'PairAuthFailed' }]);
  });

  it('waits out the pair window for welcome, then gives PairNotConfirmed', async () => {
    jest.useFakeTimers();
    try {
      const r = run(v, 'pair', { live: [] });
      await jest.advanceTimersByTimeAsync(300_000);
      expect(r.closes).toEqual([]);
      expect(r.phases).toContain('hello-sent');
      await jest.advanceTimersByTimeAsync(11_000);
      expect(r.closes).toEqual([{ kind: 'PairNotConfirmed' }]);
    } finally {
      jest.useRealTimers();
    }
  });
});

describe('any throw is a failed connection, with no key material', () => {
  const v = loadVector('kk');

  it('a malformed key is a ProtocolError', async () => {
    const r = run(v, 'link', {}, { hostKey: 'zz' });
    await flush();
    expect(r.closes).toEqual([{ kind: 'ProtocolError' }]);
  });

  it('a low-order host key (a plain Error from noble) is a ProtocolError', async () => {
    const r = run(v, 'link', {}, { hostKey: '00'.repeat(32) });
    await flush();
    expect(r.closes).toEqual([{ kind: 'ProtocolError' }]);
    expect(JSON.stringify(r.closes)).not.toContain(v.client_static_secret_hex);
  });

  it('a missing WebSocket is reported after connect returns', async () => {
    const g = globalThis as { WebSocket?: unknown };
    const saved = g.WebSocket;
    g.WebSocket = undefined;
    try {
      const r = run(v, 'link', {}, { WebSocketImpl: undefined });
      expect(r.closes).toEqual([]);
      await flush();
      expect(r.closes).toEqual([{ kind: 'ProtocolError' }]);
    } finally {
      g.WebSocket = saved;
    }
  });
});

describe('clipUtf8', () => {
  it('never splits a code point', () => {
    expect(clipUtf8('abc', 2)).toBe('ab');
    expect(clipUtf8('aé', 2)).toBe('a');
    expect(clipUtf8('😀😀', 5)).toBe('😀');
  });
});
