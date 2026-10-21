import { getDraft, setDraft, clearDrafts } from '../inbox/drafts';
import { LinkController, backoffDelay } from '../link/controller';
import type { Deps } from '../link/controller';
import type { PairingOffer } from '../link/offer';
import type { HostRecord } from '../link/store';
import {
  eventMsg,
  fakeHost,
  inboxItem,
  loadVector,
  sessionOf,
  snapshotMsg,
  vectorHostId,
  vectorRandom,
  welcomeMsg,
} from './fake-host';
import type { Script, Vector, Vectors } from './fake-host';

const ADDR = '192.168.1.20:47800';

function recordOf(v: Vector): HostRecord {
  return {
    schema: 1,
    host_id: vectorHostId(v),
    host_name: 'Vector desktop',
    host_key: v.host_static_public_hex,
    addrs: [ADDR],
    client_secret_key: v.client_static_secret_hex,
    client_public_key: v.client_static_public_hex,
    fingerprint: '0000-0000-0000-0000',
    paired_at: 1,
  };
}

function setup(
  v: Vector,
  scripts: Script[],
  o: {
    hosts?: HostRecord[];
    online?: boolean;
    pairing?: boolean;
    jitter?: number;
    vectors?: Vectors;
    random?: Deps['random'];
  } = {},
) {
  const host = fakeHost(o.vectors ?? v, ...scripts);
  const saved: HostRecord[] = [];
  const forgotten: string[] = [];
  const hosts = o.hosts ?? [];
  let netCb: (online: boolean) => void = () => undefined;
  let fgCb: () => void = () => undefined;
  const deps: Deps = {
    store: {
      listHosts: async () => hosts,
      saveHost: async (r) => {
        saved.push(r);
      },
      forgetHost: async (id) => {
        forgotten.push(id);
      },
    },
    random: o.random ?? vectorRandom(v, o.pairing ?? false),
    clientName: 'Vector phone',
    appVersion: '1.0.0',
    WebSocketImpl: host.WebSocketImpl,
    isOnline: async () => o.online ?? true,
    onNetwork: (cb) => {
      netCb = cb;
      return () => undefined;
    },
    onForeground: (cb) => {
      fgCb = cb;
      return () => undefined;
    },
    jitter: () => o.jitter ?? 0,
  };
  const ctl = new LinkController(deps);
  return {
    ctl,
    host,
    saved,
    forgotten,
    network: (online: boolean) => netCb(online),
    foreground: () => fgCb(),
  };
}

const tick = (ms: number) => jest.advanceTimersByTimeAsync(ms);

beforeEach(() => jest.useFakeTimers());
afterEach(() => jest.useRealTimers());

describe('keepalive', () => {
  const v = loadVector('kk');

  it('goes live and keeps the snapshot', async () => {
    const t = setup(v, [{}], { hosts: [recordOf(v)] });
    await t.ctl.start();
    await tick(0);
    const s = t.ctl.getState();
    expect(s.phase).toBe('live');
    expect(s.host?.host_id).toBe(vectorHostId(v));
    expect(s.data.sessions.map((x) => x.id)).toEqual(['s1']);
    expect(s.data.hostName).toBe('Vector desktop');
    expect(s.error).toBeNull();
    expect(JSON.stringify(s.host)).not.toContain(v.client_static_secret_hex);
    t.ctl.dispose();
  });

  it('pings by 25 s, always before the host 60 s limit', async () => {
    const t = setup(v, [{}], { hosts: [recordOf(v)] });
    await t.ctl.start();
    await tick(0);
    const sock = t.host.sockets[0];
    await tick(24_999);
    expect(sock.pings).toBe(0);
    await tick(1);
    expect(sock.pings).toBe(1);
    for (let i = 2; i <= 6; i++) {
      await tick(25_000);
      expect(sock.pings).toBe(i);
    }
    expect(t.ctl.getState().phase).toBe('live');
    expect(t.host.sockets.length).toBe(1);
    t.ctl.dispose();
  });

  it('reconnects when no pong arrives within 15 s', async () => {
    const t = setup(v, [{ noPong: true }, {}], { hosts: [recordOf(v)] });
    await t.ctl.start();
    await tick(0);
    await tick(25_000 + 14_999);
    expect(t.host.sockets.length).toBe(1);
    expect(t.ctl.getState().phase).toBe('live');
    await tick(1);
    expect(t.host.sockets[0].closed).toBe(true);
    expect(t.ctl.getState().phase).toBe('waiting');
    await tick(1_000);
    expect(t.host.sockets.length).toBe(2);
    expect(t.ctl.getState().phase).toBe('live');
    t.ctl.dispose();
  });

  it('counts any host message as proof of life', async () => {
    const t = setup(v, [{ noPong: true }], { hosts: [recordOf(v)] });
    await t.ctl.start();
    await tick(0);
    await tick(25_000);
    t.host.sockets[0].pushHost(eventMsg(2, 'session-updated', sessionOf('s1', 'idle')));
    await tick(14_000);
    expect(t.ctl.getState().phase).toBe('live');
    expect(t.ctl.getState().data.sessions[0].status).toBe('idle');
    t.ctl.dispose();
  });
});

describe('backoff and rate', () => {
  const v = loadVector('kk');
  const dead: Script = { openFail: { error: 'connect failed' } };

  async function attemptTimes(jitter: number, totalMs: number): Promise<number[]> {
    const t = setup(v, [dead], { hosts: [recordOf(v)], jitter });
    const times: number[] = [];
    const t0 = Date.now();
    await t.ctl.start();
    let seen = 0;
    for (let at = 0; at < totalMs; at += 10) {
      await tick(10);
      while (seen < t.host.sockets.length) {
        times.push(Date.now() - t0);
        seen += 1;
      }
    }
    t.ctl.dispose();
    return times;
  }

  it('waits 1, 2, 4, 8, 16, then 30 s, never shrinking', async () => {
    const times = await attemptTimes(0, 200_000);
    const gaps = times.slice(1).map((x, i) => x - times[i]);
    const round = gaps.map((g) => Math.round(g / 100) * 100);
    expect(round.slice(0, 8)).toEqual([1000, 2000, 4000, 8000, 16000, 30000, 30000, 30000]);
  });

  it('with the most jitter it still never shrinks and never passes 30 s', async () => {
    const times = await attemptTimes(0.999, 200_000);
    const gaps = times.slice(1).map((x, i) => x - times[i]);
    for (let i = 1; i < gaps.length; i++) expect(gaps[i]).toBeGreaterThanOrEqual(gaps[i - 1] - 20);
    expect(Math.max(...gaps)).toBeLessThanOrEqual(30_020);
    expect(gaps[0]).toBeGreaterThan(1_100);
  });

  it('backoffDelay is monotone with jitter in [0, 1)', () => {
    for (const j of [0, 0.5, 0.999]) {
      const d = [0, 1, 2, 3, 4, 5, 6, 9].map((i) => backoffDelay(i, j));
      for (let i = 1; i < d.length; i++) expect(d[i]).toBeGreaterThanOrEqual(d[i - 1]);
      expect(d[d.length - 1]).toBe(30_000);
    }
  });

  it('never starts more than 10 attempts in any 60 s, even when triggers pile up', async () => {
    const t = setup(v, [dead], { hosts: [recordOf(v)] });
    const times: number[] = [];
    const t0 = Date.now();
    await t.ctl.start();
    let seen = 0;
    for (let at = 0; at < 130_000; at += 250) {
      t.foreground();
      t.network(false);
      t.network(true);
      t.ctl.retryNow();
      await tick(250);
      while (seen < t.host.sockets.length) {
        times.push(Date.now() - t0);
        seen += 1;
      }
    }
    expect(times.length).toBeGreaterThan(10);
    for (let i = 0; i + 10 < times.length; i++) {
      expect(times[i + 10] - times[i]).toBeGreaterThanOrEqual(60_000 - 300);
    }
    t.ctl.dispose();
  }, 60_000);

  it('makes no attempt while offline, and one at once when the network returns', async () => {
    const t = setup(v, [{}], { hosts: [recordOf(v)], online: false });
    await t.ctl.start();
    await tick(120_000);
    expect(t.host.sockets.length).toBe(0);
    expect(t.ctl.getState().phase).toBe('offline');
    t.network(true);
    await tick(0);
    expect(t.host.sockets.length).toBe(1);
    expect(t.ctl.getState().phase).toBe('live');
    t.ctl.dispose();
  });

  it('closes a live link when the network goes, and stays quiet until it returns', async () => {
    const t = setup(v, [{}], { hosts: [recordOf(v)] });
    await t.ctl.start();
    await tick(0);
    t.network(false);
    await tick(120_000);
    expect(t.host.sockets.length).toBe(1);
    expect(t.host.sockets[0].closed).toBe(true);
    expect(t.ctl.getState().phase).toBe('offline');
    t.network(true);
    await tick(0);
    expect(t.host.sockets.length).toBe(2);
    expect(t.ctl.getState().phase).toBe('live');
    t.ctl.dispose();
  });

  it('retries at once on app foreground and on Try now', async () => {
    const t = setup(v, [dead, dead, {}], { hosts: [recordOf(v)] });
    await t.ctl.start();
    await tick(1_100);
    expect(t.host.sockets.length).toBe(2);
    expect(t.ctl.getState().phase).toBe('waiting');
    t.foreground();
    await tick(0);
    expect(t.host.sockets.length).toBe(3);
    expect(t.ctl.getState().phase).toBe('live');
    t.ctl.dispose();
  });
});

describe('failure rules', () => {
  const v = loadVector('kk');

  it('two LinkRefused in a row give NotPaired, with the keys still stored', async () => {
    const t = setup(v, [{ silentAfter: 'msg1' }], { hosts: [recordOf(v)] });
    await t.ctl.start();
    await tick(0);
    expect(t.ctl.getState().phase).toBe('waiting');
    expect(t.ctl.getState().error).toEqual({ kind: 'LinkRefused' });
    await tick(1_000);
    expect(t.ctl.getState().phase).toBe('not_paired');
    expect(t.forgotten).toEqual([]);
    expect(t.ctl.getState().host).not.toBeNull();
    // It keeps trying, at the slowest backoff.
    await tick(30_000);
    expect(t.host.sockets.length).toBe(3);
    expect(t.ctl.getState().phase).toBe('not_paired');
    t.ctl.dispose();
  });

  it('a success between two refusals resets the count', async () => {
    const t = setup(v, [{ silentAfter: 'msg1' }, {}, { silentAfter: 'msg1' }], {
      hosts: [recordOf(v)],
    });
    await t.ctl.start();
    await tick(0);
    await tick(1_000);
    expect(t.ctl.getState().phase).toBe('live');
    t.host.sockets[1].closeFromHost();
    await tick(1_000);
    expect(t.ctl.getState().phase).toBe('waiting');
    expect(t.forgotten).toEqual([]);
    t.ctl.dispose();
  });

  it('Revoked calls forgetHost and returns to unpaired', async () => {
    const revoked = { v: 1, type: 'error', code: 'revoked', message: 'revoked' };
    const t = setup(v, [{ live: [revoked] }], { hosts: [recordOf(v)] });
    await t.ctl.start();
    await tick(0);
    expect(t.forgotten).toEqual([vectorHostId(v)]);
    const s = t.ctl.getState();
    expect(s.phase).toBe('unpaired');
    expect(s.host).toBeNull();
    expect(s.error).toEqual({ kind: 'Revoked' });
    await tick(60_000);
    expect(t.host.sockets.length).toBe(1);
    t.ctl.dispose();
  });

  it('an unreachable host keeps its keys and backs off', async () => {
    const t = setup(v, [{ openFail: { error: 'x' } }], { hosts: [recordOf(v)] });
    await t.ctl.start();
    await tick(0);
    expect(t.ctl.getState().phase).toBe('waiting');
    expect(t.ctl.getState().error).toEqual({ kind: 'Unreachable' });
    expect(t.forgotten).toEqual([]);
    t.ctl.dispose();
  });

  it('a version mismatch is shown and retried', async () => {
    const reply = JSON.stringify({
      type: 'error',
      v: 1,
      code: 'unsupported_version',
      message: 'no',
      supported: [2],
    });
    const t = setup(v, [{ reply }], { hosts: [recordOf(v)] });
    await t.ctl.start();
    await tick(0);
    expect(t.ctl.getState().error).toEqual({ kind: 'VersionMismatch', supported: [2] });
    expect(t.ctl.getState().phase).toBe('waiting');
    t.ctl.dispose();
  });

  it('a seq gap reconnects at once, once; a repeat backs off', async () => {
    const gap = [welcomeMsg(v), snapshotMsg(1), eventMsg(3, 'session-updated', sessionOf('s2'))];
    const t = setup(v, [{ live: gap }], { hosts: [recordOf(v)] });
    await t.ctl.start();
    await tick(0);
    await tick(10);
    expect(t.host.sockets.length).toBe(2);
    await tick(10);
    expect(t.host.sockets.length).toBe(2);
    await tick(1_100);
    expect(t.host.sockets.length).toBe(3);
    t.ctl.dispose();
  });

  it('a TEXT frame after the handshake reconnects and does not delete keys', async () => {
    const t = setup(v, [{ textAfterHello: '{}' }, {}], { hosts: [recordOf(v)] });
    await t.ctl.start();
    await tick(10);
    expect(t.host.sockets.length).toBe(2);
    expect(t.ctl.getState().phase).toBe('live');
    expect(t.forgotten).toEqual([]);
    t.ctl.dispose();
  });

  it('forget deletes the record and returns to unpaired', async () => {
    const t = setup(v, [{}], { hosts: [recordOf(v)] });
    await t.ctl.start();
    await tick(0);
    await t.ctl.forget();
    expect(t.forgotten).toEqual([vectorHostId(v)]);
    expect(t.ctl.getState().phase).toBe('unpaired');
    expect(t.ctl.getState().data.sessions).toEqual([]);
    expect(t.host.sockets[0].closed).toBe(true);
    await tick(60_000);
    expect(t.host.sockets.length).toBe(1);
    t.ctl.dispose();
  });

  it('with no stored host it is unpaired and connects to nothing', async () => {
    const t = setup(v, [{}]);
    await t.ctl.start();
    await tick(60_000);
    expect(t.ctl.getState().phase).toBe('unpaired');
    expect(t.host.sockets.length).toBe(0);
    t.ctl.dispose();
  });
});

describe('pairing', () => {
  const v = loadVector('ikpsk2');
  const offer = (): PairingOffer => ({
    v: 1,
    host_id: vectorHostId(v),
    host_name: 'Vector desktop',
    host_key: v.host_static_public_hex,
    secret: v.psk_hex!,
    addrs: [ADDR],
    expires_at: Date.now() + 300_000,
  });

  it('saves the host only after welcome', async () => {
    const t = setup(v, [{ live: [] }], { pairing: true });
    await t.ctl.start();
    const p = t.ctl.pair(offer());
    await tick(0);
    const waiting = t.ctl.getState();
    expect(waiting.phase).toBe('pairing');
    expect(waiting.pairStep).toBe('confirm');
    expect(waiting.pairFingerprint).toMatch(/^[0-9a-f]{4}(-[0-9a-f]{4}){3}$/);
    await tick(120_000);
    expect(t.saved).toEqual([]);

    t.host.sockets[0].pushHost(welcomeMsg(v));
    t.host.sockets[0].pushHost(snapshotMsg(1));
    await tick(0);
    const out = await p;
    expect(out.ok).toBe(true);
    expect(t.saved.length).toBe(1);
    expect(t.saved[0]).toMatchObject({
      schema: 1,
      host_id: vectorHostId(v),
      host_name: 'Vector desktop',
      host_key: v.host_static_public_hex,
      addrs: [ADDR],
      client_secret_key: v.client_static_secret_hex,
      client_public_key: v.client_static_public_hex,
    });
    expect(t.ctl.getState().phase).toBe('live');
    expect(t.ctl.getState().host?.host_id).toBe(vectorHostId(v));
    expect(t.ctl.getState().data.sessions.length).toBe(1);
    // The pairing connection carries on as the live link; no second connection is made.
    expect(t.host.sockets.length).toBe(1);
    t.ctl.dispose();
  });

  it('never saves when the desktop does not confirm', async () => {
    const t = setup(v, [{ silentAfter: 'hello' }], { pairing: true });
    await t.ctl.start();
    const out = await Promise.all([t.ctl.pair(offer()), tick(0)]).then(([o]) => o);
    expect(out).toEqual({ ok: false, failure: { kind: 'PairNotConfirmed' } });
    expect(t.saved).toEqual([]);
    expect(t.ctl.getState().phase).toBe('unpaired');
    expect(t.ctl.getState().error).toEqual({ kind: 'PairNotConfirmed' });
    t.ctl.dispose();
  });

  it('a wrong code gives PairAuthFailed and saves nothing', async () => {
    const t = setup(v, [{ flipMsg2: true }], { pairing: true });
    await t.ctl.start();
    const out = await Promise.all([t.ctl.pair(offer()), tick(0)]).then(([o]) => o);
    expect(out).toEqual({ ok: false, failure: { kind: 'PairAuthFailed' } });
    expect(t.saved).toEqual([]);
    t.ctl.dispose();
  });

  it('a closed window gives PairWindowClosed', async () => {
    const t = setup(v, [{ openFail: { reason: "was '404 Not Found'" } }], { pairing: true });
    await t.ctl.start();
    const out = await Promise.all([t.ctl.pair(offer()), tick(0)]).then(([o]) => o);
    expect(out).toEqual({ ok: false, failure: { kind: 'PairWindowClosed' } });
    t.ctl.dispose();
  });

  it('tries the next address when the first is unreachable', async () => {
    const t = setup(v, [{ openFail: { error: 'x' } }, { live: [] }], { pairing: true });
    await t.ctl.start();
    const p = t.ctl.pair({ ...offer(), addrs: ['10.0.0.1:47800', '10.0.0.2:47800'] });
    await tick(0);
    expect(t.host.sockets.map((s) => s.url)).toEqual([
      'ws://10.0.0.1:47800/pair',
      'ws://10.0.0.2:47800/pair',
    ]);
    t.ctl.cancelPair();
    await tick(0);
    await p;
    t.ctl.dispose();
  });

  it('cancelPair closes the socket and saves nothing', async () => {
    const t = setup(v, [{ live: [] }], { pairing: true });
    await t.ctl.start();
    const p = t.ctl.pair(offer());
    await tick(0);
    t.ctl.cancelPair();
    await tick(0);
    const out = await p;
    expect(out.ok).toBe(false);
    expect(t.host.sockets[0].closed).toBe(true);
    expect(t.saved).toEqual([]);
    expect(t.ctl.getState().phase).toBe('unpaired');
    expect(t.ctl.getState().error).toBeNull();
    t.ctl.dispose();
  });

  // A stored host dials /link (KK) first, then the offer dials /pair (IKpsk2). The draws are
  // the link ephemeral, then the fresh static and the pair ephemeral.
  const kk = loadVector('kk');
  const twoPaths = (hosts: HostRecord[], scripts: Script[]) => {
    const eph = vectorRandom(v, false);
    const stat = vectorRandom(v, true);
    let calls = 0;
    return setup(v, scripts, {
      hosts,
      vectors: { pair: v, link: kk },
      random: (n) => {
        calls += 1;
        return calls === 2 ? stat(n) : eph(n);
      },
    });
  };

  it('a stored host and a pairing offer each get their own vector', async () => {
    const t = twoPaths([recordOf(kk)], [{}, { live: [] }]);
    await t.ctl.start();
    await tick(0);
    expect(t.ctl.getState().phase).toBe('live');
    const p = t.ctl.pair(offer());
    await tick(0);
    const [link, pair] = t.host.sockets;
    expect(link.url.endsWith('/link')).toBe(true);
    expect(link.msg1Matches).toBe(true);
    expect(pair.url.endsWith('/pair')).toBe(true);
    expect(pair.msg1Matches).toBe(true);
    t.ctl.cancelPair();
    await tick(0);
    await p;
    t.ctl.dispose();
  });

  it('dispose during pair with a stored host leaves nothing running', async () => {
    const t = twoPaths([recordOf(kk)], [{}, { live: [] }]);
    await t.ctl.start();
    await tick(0);
    expect(t.ctl.getState().phase).toBe('live');
    expect(t.host.sockets[0].url.endsWith('/link')).toBe(true);
    expect(t.host.sockets[0].msg1Matches).toBe(true);
    const p = t.ctl.pair(offer());
    await tick(0);
    expect(t.ctl.getState().pairStep).toBe('confirm');
    t.ctl.dispose();
    await p;
    const before = t.host.sockets.length;
    const pings = () => t.host.sockets.reduce((n, s) => n + s.pings, 0);
    const pingsBefore = pings();
    // The setup unsubscribes are no-ops, so these late callbacks still reach attempt().
    t.ctl.retryNow();
    t.foreground();
    t.network(false);
    t.network(true);
    await tick(180_000);
    expect(t.host.sockets.length).toBe(before);
    expect(jest.getTimerCount()).toBe(0);
    expect(pings()).toBe(pingsBefore);
  });

  it('cancelPair while the host is being saved keeps nothing', async () => {
    const t = setup(v, [{}], { pairing: true });
    let release: () => void = () => undefined;
    (t.ctl as unknown as { deps: Deps }).deps.store.saveHost = (r) =>
      new Promise<void>((res) => {
        t.saved.push(r);
        release = res;
      });
    await t.ctl.start();
    const p = t.ctl.pair(offer());
    await tick(0);
    expect(t.saved.length).toBe(1);
    t.ctl.cancelPair();
    release();
    const out = await p;
    expect(out).toEqual({ ok: false, failure: { kind: 'Closed' } });
    expect(t.forgotten).toEqual([vectorHostId(v)]);
    expect(t.ctl.getState().phase).toBe('unpaired');
    expect(t.ctl.getState().host).toBeNull();
    t.ctl.dispose();
  });

  // Holds the first saveHost (the new record) until released; later calls (a restore) go
  // through at once, or throw when `restoreFails`.
  const holdFirstSave = (t: ReturnType<typeof setup>, restoreFails = false) => {
    let release: () => void = () => undefined;
    let calls = 0;
    (t.ctl as unknown as { deps: Deps }).deps.store.saveHost = (r) => {
      t.saved.push(r);
      calls += 1;
      if (calls === 1) {
        return new Promise<void>((res) => {
          release = res;
        });
      }
      return restoreFails ? Promise.reject(new Error('disk full')) : Promise.resolve();
    };
    return () => release();
  };

  it('cancel while re-pairing a stored host restores its previous record', async () => {
    const previous = recordOf(kk);
    const t = twoPaths([previous], [{}, {}]);
    const release = holdFirstSave(t);
    await t.ctl.start();
    await tick(0);
    expect(t.ctl.getState().phase).toBe('live');
    const p = t.ctl.pair(offer());
    await tick(0);
    expect(t.saved.length).toBe(1);
    expect(t.saved[0]).not.toEqual(previous);
    t.ctl.cancelPair();
    release();
    const out = await p;
    expect(out).toEqual({ ok: false, failure: { kind: 'Closed' } });
    expect(t.saved.length).toBe(2);
    expect(t.saved[t.saved.length - 1]).toEqual(previous);
    expect(t.forgotten).toEqual([]);
    expect(t.ctl.getState().host?.host_id).toBe(previous.host_id);
    t.ctl.dispose();
  });

  it('cancel while pairing a new host beside a stored one forgets only the new one', async () => {
    // The stored host appears after start, so only the pairing dials.
    const stored = { ...recordOf(kk), host_id: 'a'.repeat(32) };
    const hosts: HostRecord[] = [];
    const t = setup(v, [{}], { pairing: true, hosts });
    const release = holdFirstSave(t);
    await t.ctl.start();
    hosts.push(stored);
    const p = t.ctl.pair(offer());
    await tick(0);
    expect(t.saved.length).toBe(1);
    t.ctl.cancelPair();
    release();
    await p;
    expect(t.saved.length).toBe(1);
    expect(t.forgotten).toEqual([vectorHostId(v)]);
    t.ctl.dispose();
  });

  it('a failed restore deletes nothing', async () => {
    const t = twoPaths([recordOf(kk)], [{}, {}]);
    const release = holdFirstSave(t, true);
    await t.ctl.start();
    await tick(0);
    const p = t.ctl.pair(offer());
    await tick(0);
    t.ctl.cancelPair();
    release();
    const out = await p;
    expect(out).toEqual({ ok: false, failure: { kind: 'Closed' } });
    expect(t.saved.length).toBe(2);
    expect(t.forgotten).toEqual([]);
    t.ctl.dispose();
  });

  it('a failed save closes the link and reports it', async () => {
    const t = setup(v, [{}], { pairing: true });
    (t.ctl as unknown as { deps: Deps }).deps.store.saveHost = async () => {
      throw new Error('disk full');
    };
    await t.ctl.start();
    const out = await Promise.all([t.ctl.pair(offer()), tick(0)]).then(([o]) => o);
    expect(out).toEqual({ ok: false, failure: { kind: 'StoreFailed' } });
    expect(t.host.sockets[0].closed).toBe(true);
    t.ctl.dispose();
  });
});

describe('which stored host is loaded', () => {
  const kk = loadVector('kk');
  const ID_A = 'a'.repeat(32);
  const ID_B = 'b'.repeat(32);
  const at = (host_id: string, paired_at: number): HostRecord => ({ ...recordOf(kk), host_id, paired_at });

  it('loads the most recently paired of two stored hosts', async () => {
    for (const hosts of [
      [at(ID_A, 5), at(ID_B, 9)],
      [at(ID_B, 9), at(ID_A, 5)],
    ]) {
      const t = setup(kk, [{}], { hosts });
      await t.ctl.start();
      expect(t.ctl.getState().host?.host_id).toBe(ID_B);
      t.ctl.dispose();
    }
  });

  it('a tie in paired_at goes to the later index entry', async () => {
    const t = setup(kk, [{}], { hosts: [at(ID_A, 7), at(ID_B, 7)] });
    await t.ctl.start();
    expect(t.ctl.getState().host?.host_id).toBe(ID_B);
    t.ctl.dispose();
  });
});

describe('answers', () => {
  const kk = loadVector('kk');
  const item = (id: string, over: Record<string, unknown> = {}) => ({ ...inboxItem(id), ...over });
  const allow = (id: string) => ({ kind: 'permission' as const, item_id: id, decision: 'allow' as const });
  const snap = (...inbox: unknown[]) => snapshotMsg(1, { inbox });
  const live = (...inbox: unknown[]): Script => ({ live: [welcomeMsg(kk), snap(...inbox)] });
  const ok = (id: string) => ({ v: 1, type: 'result', id, ok: null });
  const err = (id: string, code: string, message = 'm') => ({ v: 1, type: 'result', id, error: { code, message } });

  // The vector's key bytes for the handshake, counted bytes for call ids.
  function randomWithIds(): Deps['random'] {
    const keys = vectorRandom(kk, false);
    let n = 0;
    return (length) => {
      if (length === 32) return keys(length);
      n += 1;
      return new Uint8Array(length).fill(n);
    };
  }

  async function goLive(scripts: Script[]) {
    const t = setup(kk, scripts, { hosts: [recordOf(kk)], random: randomWithIds() });
    await t.ctl.start();
    await tick(0);
    expect(t.ctl.getState().phase).toBe('live');
    return t;
  }

  const outbox = (t: { ctl: LinkController }) => t.ctl.getState().outbox;

  it('queued offline, then sent once after the snapshot', async () => {
    const t = await goLive([live(item('i1')), { live: [welcomeMsg(kk)] }]);
    t.host.sockets[0].closeFromHost();
    await tick(0);
    t.ctl.answer(allow('i1'));
    expect(outbox(t).i1.status).toBe('queued');
    expect(t.host.sockets[0].calls).toEqual([]);
    await tick(1_000);
    // Live after welcome, but no snapshot yet: nothing goes out.
    expect(t.ctl.getState().phase).toBe('live');
    expect(t.host.sockets[1].calls).toEqual([]);
    t.host.sockets[1].pushHost(snap(item('i1')));
    await tick(0);
    const calls = t.host.sockets[1].calls;
    expect(calls.length).toBe(1);
    expect(calls[0].method).toBe('answer');
    expect(calls[0].args).toEqual(allow('i1'));
    expect(calls[0].id).toMatch(/^a-[0-9a-f]{32}$/);
    await tick(5_000);
    expect(t.host.sockets[1].calls.length).toBe(1);
    expect(outbox(t).i1.status).toBe('sending');
    t.ctl.dispose();
  });

  it('a result routes to its entry by id, an unknown id is ignored', async () => {
    const t = await goLive([live(item('i1'))]);
    t.ctl.answer(allow('i1'));
    await tick(0);
    const id = t.host.sockets[0].calls[0].id;
    const before = outbox(t);
    t.host.sockets[0].pushHost(ok('a-unknown'));
    t.host.sockets[0].pushHost(err('a-other', 'failed'));
    await tick(0);
    expect(outbox(t)).toBe(before);
    t.host.sockets[0].pushHost(ok(id));
    await tick(0);
    expect(outbox(t).i1.status).toBe('sent');
    t.ctl.dispose();
  });

  it('a drop mid-call resends the SAME id after the next snapshot', async () => {
    const t = await goLive([live(item('i1'))]);
    t.ctl.answer(allow('i1'));
    await tick(0);
    const id = t.host.sockets[0].calls[0].id;
    t.host.sockets[0].closeFromHost();
    await tick(0);
    expect(outbox(t).i1).toMatchObject({ status: 'queued', call_id: id });
    await tick(1_000);
    expect(t.host.sockets[1].calls.map((c) => c.id)).toEqual([id]);
    expect(outbox(t).i1).toMatchObject({ status: 'sending', attempt: 2 });
    t.ctl.dispose();
  });

  it('failed on a resend goes unconfirmed, and Try again keeps the id', async () => {
    const t = await goLive([live(item('i1'))]);
    t.ctl.answer(allow('i1'));
    await tick(0);
    const id = t.host.sockets[0].calls[0].id;
    t.host.sockets[0].closeFromHost();
    await tick(1_000);
    t.host.sockets[1].pushHost(err(id, 'failed', 'whatever the host says'));
    await tick(0);
    expect(outbox(t).i1.status).toBe('unconfirmed');
    t.ctl.retryAnswer('i1');
    await tick(0);
    expect(t.host.sockets[1].calls.map((c) => c.id)).toEqual([id, id]);
    t.ctl.dispose();
  });

  it('Cancel frees an unconfirmed replayed failure, a late result is ignored, a new answer gets a new id', async () => {
    const t = await goLive([live(item('i1'))]);
    t.ctl.answer(allow('i1'));
    await tick(0);
    const id = t.host.sockets[0].calls[0].id;
    t.host.sockets[0].closeFromHost();
    await tick(1_000);
    t.host.sockets[1].pushHost(err(id, 'failed', 'replayed'));
    await tick(0);
    expect(outbox(t).i1.status).toBe('unconfirmed');
    t.ctl.retryAnswer('i1');
    await tick(0);
    t.host.sockets[1].pushHost(err(id, 'failed', 'replayed'));
    await tick(0);
    expect(outbox(t).i1).toMatchObject({ status: 'unconfirmed', call_id: id });
    t.ctl.answer(allow('i1'));
    expect(outbox(t).i1.call_id).toBe(id);
    t.ctl.cancelAnswer('i1');
    expect(outbox(t)).toEqual({});
    t.host.sockets[1].pushHost(ok(id));
    await tick(0);
    expect(outbox(t)).toEqual({});
    t.ctl.answer(allow('i1'));
    await tick(0);
    expect(outbox(t).i1.call_id).not.toBe(id);
    expect(outbox(t).i1.status).toBe('sending');
    t.ctl.dispose();
  });

  it('pair empties the outbox', async () => {
    const t = await goLive([live(item('i1'))]);
    t.ctl.answer(allow('i1'));
    await tick(0);
    expect(outbox(t).i1).toBeDefined();
    const pv = loadVector('ikpsk2');
    void t.ctl.pair({
      v: 1,
      host_id: vectorHostId(pv),
      host_name: 'Other desktop',
      host_key: pv.host_static_public_hex,
      secret: pv.psk_hex!,
      addrs: [ADDR],
      expires_at: Date.now() + 300_000,
    });
    expect(outbox(t)).toEqual({});
    t.ctl.dispose();
  });

  it('a definitive failed keeps the answer and Try again uses a new id', async () => {
    const t = await goLive([live(item('i1'))]);
    t.ctl.answer(allow('i1'));
    await tick(0);
    const id = t.host.sockets[0].calls[0].id;
    t.host.sockets[0].pushHost(err(id, 'failed', 'disk'));
    await tick(0);
    expect(outbox(t).i1).toMatchObject({ status: 'failed', message: 'disk' });
    t.ctl.retryAnswer('i1');
    await tick(0);
    const ids = t.host.sockets[0].calls.map((c) => c.id);
    expect(ids.length).toBe(2);
    expect(ids[1]).not.toBe(id);
    t.ctl.dispose();
  });

  it('a queued answer whose item is resolved / expired / missing becomes answered / expired / gone and is never sent', async () => {
    const t = await goLive([
      live(item('i1'), item('i2'), item('i3')),
      live(item('i1', { resolved: true }), item('i2', { resolved: true, expired: true })),
    ]);
    t.host.sockets[0].closeFromHost();
    await tick(0);
    for (const id of ['i1', 'i2', 'i3']) t.ctl.answer(allow(id));
    await tick(1_000);
    expect(outbox(t).i1.status).toBe('answered');
    expect(outbox(t).i2.status).toBe('expired');
    expect(outbox(t).i3.status).toBe('gone');
    expect(t.host.sockets[1].calls).toEqual([]);
    t.ctl.dispose();
  });

  it('an expired failed never retries', async () => {
    const t = await goLive([live(item('i1'))]);
    t.ctl.answer(allow('i1'));
    await tick(0);
    const id = t.host.sockets[0].calls[0].id;
    t.host.sockets[0].pushHost(err(id, 'failed', 'this request expired when Orchestra closed'));
    await tick(0);
    expect(outbox(t).i1.status).toBe('failed');
    t.host.sockets[0].pushHost(eventMsg(2, 'inbox-updated', [item('i1', { resolved: true, expired: true })]));
    await tick(0);
    expect(outbox(t).i1.status).toBe('expired');
    t.ctl.retryAnswer('i1');
    await tick(0);
    expect(t.host.sockets[0].calls.length).toBe(1);
    t.ctl.dispose();
  });

  it('an inbox-updated leaves a call in flight to its result', async () => {
    const t = await goLive([live(item('i1'), item('i2'))]);
    t.ctl.answer(allow('i1'));
    t.ctl.answer(allow('i2'));
    await tick(0);
    t.host.sockets[0].pushHost(eventMsg(2, 'inbox-updated', [item('i1', { resolved: true }), item('i2')]));
    await tick(0);
    expect(outbox(t).i1.status).toBe('sending');
    expect(outbox(t).i2.status).toBe('queued');
    t.ctl.dispose();
  });

  it('20 s with no result goes unconfirmed, and a late result still lands', async () => {
    const t = await goLive([live(item('i1'))]);
    t.ctl.answer(allow('i1'));
    await tick(0);
    const id = t.host.sockets[0].calls[0].id;
    await tick(19_999);
    expect(outbox(t).i1.status).toBe('sending');
    await tick(1);
    expect(outbox(t).i1).toMatchObject({ status: 'unconfirmed', call_id: id });
    t.host.sockets[0].pushHost(ok(id));
    await tick(0);
    expect(outbox(t).i1.status).toBe('sent');
    t.ctl.dispose();
  });

  it('never two answer calls in flight', async () => {
    const t = await goLive([live(item('i1'), item('i2'))]);
    t.ctl.answer(allow('i1'));
    t.ctl.answer(allow('i2'));
    await tick(0);
    const calls = t.host.sockets[0].calls;
    expect(calls.length).toBe(1);
    expect(calls[0].args.item_id).toBe('i1');
    expect(outbox(t).i2.status).toBe('queued');
    t.host.sockets[0].pushHost(ok(calls[0].id));
    await tick(0);
    expect(calls.map((c) => c.args.item_id)).toEqual(['i1', 'i2']);
    t.ctl.dispose();
  });

  it('Forget empties the outbox', async () => {
    const t = await goLive([live(item('i1'))]);
    t.ctl.answer(allow('i1'));
    await tick(0);
    await t.ctl.forget();
    expect(outbox(t)).toEqual({});
    expect(t.ctl.getState().data.inbox).toEqual([]);
    t.ctl.dispose();
  });

  it('Forget and revoke clear the drafts', async () => {
    const t = await goLive([live(item('i1'))]);
    setDraft('i1', { message: 'secret', question: { picks: {}, other: {} } });
    await t.ctl.forget();
    expect(getDraft('i1').message).toBe('');
    t.ctl.dispose();
    const r = await goLive([live(item('i1'))]);
    setDraft('i1', { message: 'secret', question: { picks: {}, other: {} } });
    r.host.sockets[0].pushHost({ v: 1, type: 'error', code: 'revoked', message: 'revoked' });
    await tick(0);
    expect(getDraft('i1').message).toBe('');
    r.ctl.dispose();
    clearDrafts();
  });

  it('revoked empties the outbox', async () => {
    const t = await goLive([live(item('i1'))]);
    t.ctl.answer(allow('i1'));
    await tick(0);
    t.host.sockets[0].pushHost({ v: 1, type: 'error', code: 'revoked', message: 'revoked' });
    await tick(0);
    expect(t.ctl.getState().phase).toBe('unpaired');
    expect(outbox(t)).toEqual({});
    t.ctl.dispose();
  });

  it('cancel removes a queued answer only, dismiss clears one that is over', async () => {
    const t = await goLive([live(item('i1'), item('i2'))]);
    t.ctl.answer(allow('i1'));
    t.ctl.answer(allow('i2'));
    await tick(0);
    t.ctl.cancelAnswer('i1');
    expect(outbox(t).i1.status).toBe('sending');
    t.ctl.cancelAnswer('i2');
    expect(outbox(t).i2).toBeUndefined();
    t.host.sockets[0].pushHost(err(t.host.sockets[0].calls[0].id, 'forbidden'));
    await tick(0);
    expect(outbox(t).i1.status).toBe('refused');
    t.ctl.dismissAnswer('i1');
    expect(outbox(t)).toEqual({});
    t.ctl.dispose();
  });

  it('a reply is refused and queues nothing', async () => {
    const t = await goLive([live(item('i1'))]);
    t.ctl.answer({ kind: 'reply', session_id: 's1', text: 'hi' });
    await tick(0);
    expect(outbox(t)).toEqual({});
    expect(t.host.sockets[0].calls).toEqual([]);
    t.ctl.dispose();
  });
});
