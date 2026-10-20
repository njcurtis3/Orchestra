// Owns the one paired host (the most recently paired of listHosts): connect, keep alive, reconnect, and the
// pairing flow. No React and no native modules; the app wires those in through Deps.
// Keys are deleted only on `revoked` or on forget (G6). Nothing here logs.
import type { Answer, HostMessage } from '../../../protocol/types';
import { toHex } from './bytes';
import { connect as realConnect } from './client';
import type { ConnectOptions, LinkConnection, LinkFailure, LinkPhase, LinkMode } from './client';
import { fingerprint, generateStaticKey } from './keys';
import type { KeyPair, Random } from './keys';
import type { PairingOffer } from './offer';
import * as box from './outbox';
import type { Outbox, OutboxAnswer, OutboxEntry } from './outbox';
import { initialData, reduce } from './state';
import type { LinkData } from './state';
import type { HostRecord } from './store';

export const PING_EVERY_MS = 25_000;
export const PONG_WITHIN_MS = 15_000;
export const BACKOFF_MS = [1_000, 2_000, 4_000, 8_000, 16_000, 30_000];
export const MAX_BACKOFF_MS = 30_000;
export const MAX_JITTER = 0.2;
export const MAX_ATTEMPTS_PER_MINUTE = 10;
const WINDOW_MS = 60_000;
// A protocol-class drop reconnects at once, but only once a minute; repeats back off.
const IMMEDIATE_ONCE_MS = 60_000;
export const LINK_REFUSED_LIMIT = 2;
// A link must stay live this long before the backoff starts over.
export const STABLE_LINK_MS = 30_000;
// A call with no result this long, on a live link, may or may not have landed.
export const CALL_RESULT_MS = 20_000;

export type ControllerPhase =
  | 'loading'
  | 'unpaired'
  | 'pairing'
  | 'connecting'
  | 'live'
  | 'offline'
  | 'waiting'
  | 'not_paired';

// What the UI may know of the host: never a key.
export type HostSummary = { host_id: string; host_name: string; fingerprint: string; addrs: string[] };

export type ControllerState = {
  phase: ControllerPhase;
  host: HostSummary | null;
  data: LinkData;
  // Answers the phone holds, by item id. In memory only.
  outbox: Outbox;
  // The last failure; cleared when a connection is live.
  error: LinkFailure | null;
  // While pairing: connecting, then the desktop confirm wait.
  pairStep: 'connecting' | 'confirm' | null;
  // The phone's own fingerprint for the confirm screen, while pairing.
  pairFingerprint: string | null;
  // When the next attempt fires, ms since the epoch, while waiting.
  retryAt: number | null;
};

export type PairOutcome =
  | { ok: true; host: HostSummary }
  | { ok: false; failure: LinkFailure | { kind: 'StoreFailed' } };

export interface HostStore {
  listHosts(): Promise<HostRecord[]>;
  saveHost(record: HostRecord): Promise<void>;
  forgetHost(hostId: string): Promise<void>;
}

export type Deps = {
  store: HostStore;
  random: Random;
  clientName: string;
  appVersion: string;
  // Defaults to the real client; tests pass a WebSocketImpl through it.
  connect?: (o: ConnectOptions) => LinkConnection;
  WebSocketImpl?: ConnectOptions['WebSocketImpl'];
  isOnline: () => Promise<boolean>;
  // Both return an unsubscribe function.
  onNetwork: (cb: (online: boolean) => void) => () => void;
  onForeground: (cb: () => void) => () => void;
  now?: () => number;
  // [0, 1); Math.random is fine for jitter, never for keys.
  jitter?: () => number;
};

export function backoffDelay(index: number, jitter: number): number {
  const base = BACKOFF_MS[Math.min(index, BACKOFF_MS.length - 1)];
  return Math.min(Math.round(base * (1 + MAX_JITTER * jitter)), MAX_BACKOFF_MS);
}

function summary(r: HostRecord): HostSummary {
  return { host_id: r.host_id, host_name: r.host_name, fingerprint: r.fingerprint, addrs: r.addrs };
}

type Dial = { ok: true } | { ok: false; failure: LinkFailure };

export class LinkController {
  private state: ControllerState = {
    phase: 'loading',
    host: null,
    data: initialData,
    outbox: {},
    error: null,
    pairStep: null,
    pairFingerprint: null,
    retryAt: null,
  };
  private readonly listeners = new Set<() => void>();
  private readonly connectFn: (o: ConnectOptions) => LinkConnection;
  private readonly now: () => number;
  private readonly jitter: () => number;
  private record: HostRecord | null = null;
  private conn: LinkConnection | null = null;
  private online = true;
  private attempting = false;
  private pairing = false;
  private pairCancelled = false;
  // Set once by dispose; after it nothing schedules, dials or listens again.
  private disposed = false;
  // Bumped whenever in-flight work must be abandoned.
  private epoch = 0;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private pingTimer: ReturnType<typeof setTimeout> | null = null;
  private pongTimer: ReturnType<typeof setTimeout> | null = null;
  private stableTimer: ReturnType<typeof setTimeout> | null = null;
  private backoffIndex = 0;
  private refused = 0;
  private lastImmediateAt = -Infinity;
  private attemptTimes: number[] = [];
  private unsubs: (() => void)[] = [];
  // Item ids in the order their answers were queued; the flush is first in, first out.
  private order: string[] = [];
  // The connection on which a snapshot has been applied. Answers go out only on it.
  private snapshotConn: LinkConnection | null = null;
  private callTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(private readonly deps: Deps) {
    this.connectFn = deps.connect ?? realConnect;
    this.now = deps.now ?? Date.now;
    this.jitter = deps.jitter ?? Math.random;
  }

  // ---- observable state ----

  getState = (): ControllerState => this.state;

  subscribe = (cb: () => void): (() => void) => {
    this.listeners.add(cb);
    return () => {
      this.listeners.delete(cb);
    };
  };

  private set(patch: Partial<ControllerState>): void {
    this.state = { ...this.state, ...patch };
    for (const cb of [...this.listeners]) cb();
  }

  // ---- lifecycle ----

  async start(): Promise<void> {
    if (this.disposed) return;
    this.unsubs.push(
      this.deps.onNetwork((online) => this.networkChanged(online)),
      this.deps.onForeground(() => this.foreground()),
    );
    const epoch = this.epoch;
    let hosts: HostRecord[] = [];
    try {
      this.online = await this.deps.isOnline();
      hosts = await this.deps.store.listHosts();
    } catch {
      // An unreadable store is treated as no host; nothing is deleted.
    }
    if (epoch !== this.epoch || this.pairing) return;
    // The most recently paired host; a tie in paired_at goes to the later index entry.
    this.record = hosts.reduce<HostRecord | null>((best, h) => (best && h.paired_at < best.paired_at ? best : h), null);
    if (!this.record) return this.set({ phase: 'unpaired', host: null });
    this.set({ host: summary(this.record) });
    this.attempt();
  }

  dispose(): void {
    this.disposed = true;
    this.epoch++;
    this.unsubs.forEach((u) => u());
    this.unsubs = [];
    this.stopLink();
    this.listeners.clear();
  }

  // The user chose Try now.
  retryNow(): void {
    if (!this.record || this.pairing || this.conn) return;
    this.backoffIndex = 0;
    this.attempt();
  }

  async forget(): Promise<void> {
    const record = this.record;
    this.epoch++;
    this.stopLink();
    this.record = null;
    this.emptyOutbox();
    this.set({
      phase: 'unpaired',
      host: null,
      data: initialData,
      outbox: {},
      error: null,
      retryAt: null,
    });
    if (record) {
      try {
        await this.deps.store.forgetHost(record.host_id);
      } catch {
        // the record stays; it is read again at the next start
      }
    }
  }

  // ---- pairing ----

  cancelPair(): void {
    if (!this.pairing) return;
    this.pairCancelled = true;
    this.conn?.close();
  }

  async pair(offer: PairingOffer): Promise<PairOutcome> {
    if (this.pairing) return { ok: false, failure: { kind: 'Closed' } };
    this.pairing = true;
    this.pairCancelled = false;
    this.epoch++;
    const epoch = this.epoch;
    this.stopLink();
    this.set({ phase: 'pairing', pairStep: 'connecting', pairFingerprint: null, error: null, retryAt: null });
    const finish = (failure: LinkFailure | { kind: 'StoreFailed' }): PairOutcome => {
      this.pairing = false;
      this.conn = null;
      const quiet = failure.kind === 'Closed' || failure.kind === 'StoreFailed';
      this.set({ pairStep: null, pairFingerprint: null, error: quiet ? null : failure });
      if (this.disposed) {
        // nothing runs after dispose
      } else if (this.record) {
        this.set({ phase: 'waiting' });
        this.attempt();
      } else {
        this.set({ phase: 'unpaired' });
      }
      return { ok: false, failure };
    };

    let keys: KeyPair;
    try {
      keys = generateStaticKey(this.deps.random);
    } catch {
      return finish({ kind: 'ProtocolError' });
    }
    const secret = toHex(keys.secret);
    const clientPublic = toHex(keys.public);
    this.set({ pairFingerprint: fingerprint(keys.public) });

    let failure: LinkFailure = { kind: 'Unreachable' };
    for (const addr of offer.addrs) {
      await this.gate();
      if (epoch !== this.epoch) return finish({ kind: 'Closed' });
      if (this.pairCancelled) return finish({ kind: 'Closed' });
      const r = await this.dial({
        mode: 'pair',
        addr,
        hostKey: offer.host_key,
        hostId: offer.host_id,
        clientSecret: secret,
        psk: offer.secret,
        epoch,
      });
      if (epoch !== this.epoch) return finish({ kind: 'Closed' });
      if (r.ok) {
        const record: HostRecord = {
          schema: 1,
          host_id: offer.host_id,
          host_name: offer.host_name,
          host_key: offer.host_key,
          addrs: offer.addrs,
          client_secret_key: secret,
          client_public_key: clientPublic,
          fingerprint: fingerprint(keys.public),
          paired_at: this.now(),
        };
        // The stored record of the same host, so a cancel after the save can put it back.
        // undefined: the lookup failed, and a cancel then leaves the new record alone.
        let previous: HostRecord | null | undefined;
        try {
          previous = (await this.deps.store.listHosts()).find((h) => h.host_id === record.host_id) ?? null;
        } catch {
          previous = undefined;
        }
        // The host is saved only now that it has sent welcome.
        try {
          await this.deps.store.saveHost(record);
        } catch {
          this.conn?.close();
          return finish({ kind: 'StoreFailed' });
        }
        if (epoch !== this.epoch) return finish({ kind: 'Closed' });
        if (this.pairCancelled) {
          // The user cancelled before the host was saved: do not keep it (G6). A host that was
          // already stored goes back to its previous record; a failed restore deletes nothing.
          this.conn?.close();
          if (previous) {
            try {
              await this.deps.store.saveHost(previous);
            } catch {
              // the new record stays
            }
          } else if (previous === null) {
            void this.deps.store.forgetHost(record.host_id).catch(() => undefined);
          }
          return finish({ kind: 'Closed' });
        }
        this.pairing = false;
        this.record = record;
        this.set({ host: summary(record), pairStep: null, pairFingerprint: null });
        if (this.conn) this.goLive();
        else this.scheduleRetry(0);
        return { ok: true, host: summary(record) };
      }
      failure = r.failure;
      if (failure.kind !== 'Unreachable') break;
    }
    return finish(failure);
  }

  // ---- connecting ----

  private clearTimers(): void {
    for (const t of [this.retryTimer, this.pingTimer, this.pongTimer, this.stableTimer, this.callTimer]) {
      if (t !== null) clearTimeout(t);
    }
    this.retryTimer = this.pingTimer = this.pongTimer = this.stableTimer = this.callTimer = null;
    this.linkDown();
  }

  // Close the socket and forget every timer; callbacks of the old socket are ignored.
  private stopLink(): void {
    this.clearTimers();
    this.attempting = false;
    const conn = this.conn;
    this.conn = null;
    conn?.close();
  }

  private sleep(ms: number): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, ms));
  }

  // At most MAX_ATTEMPTS_PER_MINUTE handshakes in any 60 s, whatever triggers them.
  private async gate(): Promise<void> {
    for (;;) {
      const now = this.now();
      this.attemptTimes = this.attemptTimes.filter((t) => t > now - WINDOW_MS);
      if (this.attemptTimes.length < MAX_ATTEMPTS_PER_MINUTE) {
        this.attemptTimes.push(now);
        return;
      }
      await this.sleep(this.attemptTimes[0] + WINDOW_MS - now + 1);
    }
  }

  private dial(p: {
    mode: LinkMode;
    addr: string;
    hostKey: string;
    hostId: string;
    clientSecret: string;
    psk?: string;
    epoch: number;
  }): Promise<Dial> {
    return new Promise((resolve) => {
      let welcomed = false;
      const conn: LinkConnection = this.connectFn({
        mode: p.mode,
        addr: p.addr,
        hostKey: p.hostKey,
        hostId: p.hostId,
        clientSecret: p.clientSecret,
        psk: p.psk,
        clientName: this.deps.clientName,
        appVersion: this.deps.appVersion,
        random: this.deps.random,
        WebSocketImpl: this.deps.WebSocketImpl,
        onPhase: (phase: LinkPhase) => {
          if (this.conn === conn && phase === 'hello-sent' && p.mode === 'pair') {
            this.set({ pairStep: 'confirm' });
          }
        },
        onMessage: (msg) => {
          if (this.conn !== conn) return;
          if (!welcomed && msg.type === 'welcome') {
            welcomed = true;
            this.apply(msg);
            resolve({ ok: true });
            return;
          }
          this.onHost(msg);
        },
        onClose: (failure) => {
          if (!welcomed) resolve({ ok: false, failure });
          if (this.conn !== conn) return;
          this.conn = null;
          if (welcomed && !this.pairing) this.dropped(failure);
        },
      });
      this.conn = conn;
    });
  }

  private apply(msg: HostMessage): boolean {
    const r = reduce(this.state.data, msg);
    if (r.data !== this.state.data) this.set({ data: r.data });
    return r.gap;
  }

  private onHost(msg: HostMessage): void {
    // Any message from the host is proof of life.
    this.clearPong();
    // A result belongs to a call, not to the host's state: it is routed by id and never reduced.
    if (msg.type === 'result') return this.onResult(msg);
    if (this.apply(msg)) {
      // A seq gap: v1 has no replay, so reconnect for a fresh snapshot.
      const conn = this.conn;
      this.conn = null;
      conn?.close();
      this.dropped({ kind: 'ProtocolError' });
      return;
    }
    const inboxChanged = msg.type === 'event' && msg.name === 'inbox-updated';
    if (msg.type === 'snapshot') this.snapshotConn = this.conn;
    if (msg.type === 'snapshot' || inboxChanged) {
      this.reconcileAll();
      this.flush();
    }
  }

  private attempt(): void {
    const record = this.record;
    if (this.disposed || !record || this.attempting || this.conn || this.pairing) return;
    if (this.retryTimer !== null) clearTimeout(this.retryTimer);
    this.retryTimer = null;
    if (!this.online) {
      this.set({ phase: 'offline', retryAt: null });
      return;
    }
    this.attempting = true;
    const epoch = this.epoch;
    this.set({ phase: 'connecting', retryAt: null });
    void this.runAttempt(record, epoch);
  }

  private async runAttempt(record: HostRecord, epoch: number): Promise<void> {
    let failure: LinkFailure = { kind: 'Unreachable' };
    for (const addr of record.addrs) {
      await this.gate();
      if (epoch !== this.epoch) return;
      const r = await this.dial({
        mode: 'link',
        addr,
        hostKey: record.host_key,
        hostId: record.host_id,
        clientSecret: record.client_secret_key,
        epoch,
      });
      if (epoch !== this.epoch) return;
      if (r.ok) {
        this.attempting = false;
        this.goLive();
        return;
      }
      failure = r.failure;
      if (failure.kind !== 'Unreachable') break;
    }
    this.attempting = false;
    this.failed(failure);
  }

  private goLive(): void {
    this.refused = 0;
    this.set({ phase: 'live', error: null, retryAt: null });
    this.flush();
    this.schedulePing();
    // Backoff starts over only for a link that stayed up; welcome-then-close keeps backing off.
    if (this.stableTimer !== null) clearTimeout(this.stableTimer);
    this.stableTimer = setTimeout(() => {
      this.stableTimer = null;
      this.backoffIndex = 0;
    }, STABLE_LINK_MS);
  }

  // A live connection ended.
  private dropped(failure: LinkFailure): void {
    this.clearTimers();
    this.failed(failure);
  }

  // An attempt or a live connection ended with this failure (G6, G9).
  private failed(failure: LinkFailure): void {
    if (this.disposed) return;
    // Only LinkRefused attempts in a row count towards NotPaired.
    if (failure.kind !== 'LinkRefused') this.refused = 0;
    this.set({ error: failure });
    switch (failure.kind) {
      case 'Revoked': {
        const record = this.record;
        this.epoch++;
        this.record = null;
        this.emptyOutbox();
        this.set({ phase: 'unpaired', host: null, data: initialData, outbox: {}, retryAt: null });
        if (record) void this.deps.store.forgetHost(record.host_id).catch(() => undefined);
        return;
      }
      case 'LinkRefused':
        this.refused += 1;
        if (this.refused >= LINK_REFUSED_LIMIT) {
          // Keys stay stored; retry at the slowest backoff.
          this.set({ phase: 'not_paired' });
          return this.scheduleRetry(MAX_BACKOFF_MS);
        }
        break;
      case 'ProtocolError': {
        const now = this.now();
        if (now - this.lastImmediateAt >= IMMEDIATE_ONCE_MS) {
          this.lastImmediateAt = now;
          return this.scheduleRetry(0);
        }
        break;
      }
      default:
        break;
    }
    const delay = backoffDelay(this.backoffIndex, this.jitter());
    this.backoffIndex += 1;
    this.scheduleRetry(delay);
  }

  private scheduleRetry(delay: number): void {
    if (this.disposed || !this.record) return;
    if (this.retryTimer !== null) clearTimeout(this.retryTimer);
    this.retryTimer = null;
    if (!this.online) {
      this.set({ phase: 'offline', retryAt: null });
      return;
    }
    if (this.state.phase !== 'not_paired') this.set({ phase: 'waiting' });
    this.set({ retryAt: this.now() + delay });
    this.retryTimer = setTimeout(() => {
      this.retryTimer = null;
      this.attempt();
    }, delay);
  }

  // ---- answers ----

  // Queue an answer for an inbox item. It goes out once the link is live and a snapshot is applied.
  answer(a: Answer): void {
    if (this.disposed || (a.kind !== 'permission' && a.kind !== 'question')) return;
    const held = this.state.outbox[a.item_id];
    if (held && ['queued', 'sending', 'unconfirmed', 'failed'].includes(held.status)) return;
    let entry: OutboxEntry;
    try {
      entry = box.enqueue(a, this.newCallId());
    } catch {
      entry = { ...box.enqueue(a, ''), status: 'failed', code: 'local', message: 'Could not make a request id.' };
    }
    if (!this.order.includes(a.item_id)) this.order.push(a.item_id);
    this.setEntry(a.item_id, entry);
    this.reconcileAll();
    this.flush();
  }

  // Cancel an answer that has not gone out.
  cancelAnswer(itemId: string): void {
    this.update(itemId, box.cancel);
  }

  // Try again: the same id from unconfirmed, a new id from failed.
  retryAnswer(itemId: string): void {
    this.reconcileAll();
    const e = this.state.outbox[itemId];
    if (!e || (e.status !== 'unconfirmed' && e.status !== 'failed')) return;
    let id = e.call_id;
    if (e.status === 'failed') {
      try {
        id = this.newCallId();
      } catch {
        return;
      }
    }
    this.setEntry(itemId, box.retry(e, id));
    this.reconcileAll();
    this.flush();
  }

  dismissAnswer(itemId: string): void {
    this.update(itemId, box.dismiss);
  }

  private newCallId(): string {
    return 'a-' + toHex(this.deps.random(16));
  }

  private setEntry(itemId: string, entry: OutboxEntry | null): void {
    const next = { ...this.state.outbox };
    if (entry) next[itemId] = entry;
    else {
      delete next[itemId];
      this.order = this.order.filter((x) => x !== itemId);
    }
    this.set({ outbox: next });
  }

  private update(itemId: string, f: (e: OutboxEntry) => OutboxEntry | null): void {
    const e = this.state.outbox[itemId];
    if (!e) return;
    const n = f(e);
    if (n !== e) this.setEntry(itemId, n);
  }

  // Compare every entry with the held inbox; when nothing changes, nobody is notified.
  private reconcileAll(): void {
    const inbox = this.state.data.inbox;
    let next: Outbox | null = null;
    for (const id of this.order) {
      const e = this.state.outbox[id];
      if (!e) continue;
      const r = box.reconcile(e, inbox);
      if (r === e) continue;
      next = next ?? { ...this.state.outbox };
      if (r) next[id] = r;
      else delete next[id];
    }
    if (!next) return;
    const kept = next;
    this.order = this.order.filter((id) => id in kept);
    this.set({ outbox: kept });
  }

  // One answer call in flight, oldest first. Only on a live link whose snapshot is applied.
  private flush(): void {
    const conn = this.conn;
    if (this.disposed || !conn || this.state.phase !== 'live' || this.snapshotConn !== conn) return;
    const outbox = this.state.outbox;
    if (this.order.some((id) => outbox[id]?.status === 'sending')) return;
    const id = this.order.find((x) => outbox[x]?.status === 'queued');
    if (id === undefined) return;
    const e = box.sending(outbox[id]);
    const args: OutboxAnswer = e.answer;
    if (!conn.send({ v: 1, type: 'call', id: e.call_id, method: 'answer', args })) return;
    this.setEntry(id, e);
    const callId = e.call_id;
    this.callTimer = setTimeout(() => {
      this.callTimer = null;
      if (this.state.outbox[id]?.call_id !== callId) return;
      this.update(id, box.timedOut);
      this.flush();
    }, CALL_RESULT_MS);
  }

  private onResult(msg: Extract<HostMessage, { type: 'result' }>): void {
    const id = this.order.find((x) => this.state.outbox[x]?.call_id === msg.id);
    // An id this phone did not send, or no longer holds, is ignored.
    if (id === undefined) return;
    const e = this.state.outbox[id];
    if (e.status === 'sending' && this.callTimer !== null) {
      clearTimeout(this.callTimer);
      this.callTimer = null;
    }
    const expired = this.state.data.inbox.some((i) => i.id === id && i.expired);
    const next = 'error' in msg ? box.resultError(e, msg.error.code, msg.error.message, expired) : box.resultOk(e);
    if (next !== e) this.setEntry(id, next);
    this.reconcileAll();
    this.flush();
  }

  // The link is gone: nothing is in flight, and a call with no result goes back to the queue.
  private linkDown(): void {
    this.snapshotConn = null;
    const outbox = this.state.outbox;
    if (!this.order.some((id) => outbox[id]?.status === 'sending')) return;
    const next = { ...outbox };
    for (const id of this.order) if (next[id]) next[id] = box.linkDropped(next[id]);
    this.set({ outbox: next });
  }

  private emptyOutbox(): void {
    this.order = [];
    if (this.callTimer !== null) clearTimeout(this.callTimer);
    this.callTimer = null;
  }

  // ---- keepalive ----

  private clearPong(): void {
    if (this.pongTimer !== null) clearTimeout(this.pongTimer);
    this.pongTimer = null;
  }

  private schedulePing(): void {
    if (this.pingTimer !== null) clearTimeout(this.pingTimer);
    this.pingTimer = setTimeout(() => {
      this.pingTimer = null;
      this.ping();
      this.schedulePing();
    }, PING_EVERY_MS);
  }

  private ping(): void {
    const conn = this.conn;
    if (!conn || this.pongTimer !== null) return;
    if (!conn.send({ v: 1, type: 'ping' })) return;
    this.pongTimer = setTimeout(() => {
      this.pongTimer = null;
      // Nothing came back: the socket is dead.
      if (this.conn !== conn) return;
      this.conn = null;
      conn.close();
      this.dropped({ kind: 'Closed' });
    }, PONG_WITHIN_MS);
  }

  // ---- network and app state ----

  private networkChanged(online: boolean): void {
    if (online === this.online) return;
    this.online = online;
    this.refused = 0;
    if (!this.record || this.pairing) return;
    if (!online) {
      this.epoch++;
      this.stopLink();
      this.set({ phase: 'offline', retryAt: null });
      return;
    }
    this.backoffIndex = 0;
    this.attempt();
  }

  private foreground(): void {
    if (!this.record || this.pairing) return;
    if (this.conn && this.state.phase === 'live') return this.ping();
    if (!this.conn) {
      this.backoffIndex = 0;
      this.attempt();
    }
  }
}
