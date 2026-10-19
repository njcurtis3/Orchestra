import { decodeHostMessage, type HostMessage } from '../../../protocol';
import { initialData, reduce } from '../link/state';
import type { LinkData } from '../link/state';
import {
  eventMsg,
  inboxItem,
  loadVector,
  sessionOf,
  snapshotMsg,
  welcomeMsg,
} from './fake-host';

const v = loadVector('kk');
const msg = (m: unknown): HostMessage => decodeHostMessage(JSON.stringify(m));

function apply(data: LinkData, ...messages: unknown[]) {
  let gap = false;
  for (const m of messages) {
    const r = reduce(data, msg(m));
    data = r.data;
    gap = gap || r.gap;
  }
  return { data, gap };
}

const start = () => apply(initialData, welcomeMsg(v), snapshotMsg(5)).data;

describe('reduce', () => {
  it('takes scopes and host name from welcome', () => {
    const d = apply(initialData, welcomeMsg(v)).data;
    expect(d.hostName).toBe('Vector desktop');
    expect(d.scopes).toEqual(['read', 'answer']);
    expect(d.lastSeq).toBeNull();
  });

  it('a snapshot replaces the lists and sets lastSeq', () => {
    const d = apply(start(), snapshotMsg(9, { sessions: [sessionOf('s2')] })).data;
    expect(d.sessions.map((s) => s.id)).toEqual(['s2']);
    expect(d.lastSeq).toBe(9);
    expect(d.scopes).toEqual(['read', 'answer']);
  });

  it('applies consecutive events after a snapshot', () => {
    const r = apply(
      start(),
      eventMsg(6, 'session-updated', sessionOf('s2')),
      eventMsg(7, 'inbox-updated', [inboxItem('i1')]),
    );
    expect(r.gap).toBe(false);
    expect(r.data.lastSeq).toBe(7);
    expect(r.data.sessions.map((s) => s.id)).toEqual(['s1', 's2']);
    expect(r.data.inbox.map((i) => i.id)).toEqual(['i1']);
  });

  it('inbox-updated replaces the whole list, it does not merge', () => {
    const d = apply(
      apply(start(), eventMsg(6, 'inbox-updated', [inboxItem('a'), inboxItem('b')])).data,
      eventMsg(7, 'inbox-updated', [inboxItem('c')]),
    ).data;
    expect(d.inbox.map((i) => i.id)).toEqual(['c']);
    expect(apply(d, eventMsg(8, 'inbox-updated', [])).data.inbox).toEqual([]);
  });

  it('session-updated upserts one item and leaves the others', () => {
    const d1 = apply(start(), eventMsg(6, 'session-updated', sessionOf('s2'))).data;
    const d2 = apply(d1, eventMsg(7, 'session-updated', sessionOf('s1', 'idle'))).data;
    expect(d2.sessions.map((s) => [s.id, s.status])).toEqual([
      ['s1', 'idle'],
      ['s2', 'working'],
    ]);
  });

  it('task-updated upserts by id', () => {
    const task = (status: string) => ({
      id: 't1',
      project_id: 'p1',
      project_name: 'P',
      status,
      title: 'T',
      created_at: 1,
      session_id: null,
      branch: null,
      base_branch: 'main',
      note: null,
      last_turn_error: false,
      session_over: false,
    });
    const d1 = apply(start(), eventMsg(6, 'task-updated', task('queued'))).data;
    const d2 = apply(d1, eventMsg(7, 'task-updated', task('running'))).data;
    expect(d2.tasks.map((t) => t.status)).toEqual(['running']);
  });

  it('a seq gap is reported and nothing is applied', () => {
    const before = start();
    const r = apply(before, eventMsg(7, 'session-updated', sessionOf('s9')));
    expect(r.gap).toBe(true);
    expect(r.data).toBe(before);
  });

  it('a seq at or below lastSeq is ignored', () => {
    const before = start();
    for (const seq of [5, 4, 0]) {
      const r = apply(before, eventMsg(seq, 'session-updated', sessionOf('s9')));
      expect(r.gap).toBe(false);
      expect(r.data).toBe(before);
    }
  });

  it('an event before any snapshot is dropped', () => {
    const r = apply(initialData, eventMsg(1, 'session-updated', sessionOf('s1')));
    expect(r.gap).toBe(false);
    expect(r.data.sessions).toEqual([]);
  });

  it('transcript-appended and unknown events only advance seq', () => {
    const d = apply(
      start(),
      eventMsg(6, 'transcript-appended', { session_id: 's1', entries: [] }),
      eventMsg(7, 'notice', { anything: true }),
      eventMsg(8, 'brand-new-event', 42),
    ).data;
    expect(d.lastSeq).toBe(8);
    expect(d.sessions.map((s) => s.id)).toEqual(['s1']);
  });

  it('keeps an unknown status string as sent', () => {
    const d = apply(start(), eventMsg(6, 'session-updated', sessionOf('s1', 'dreaming'))).data;
    expect(d.sessions[0].status).toBe('dreaming');
  });

  it('ignores pong and result', () => {
    const before = start();
    expect(apply(before, { v: 1, type: 'pong' }).data).toBe(before);
    expect(apply(before, { v: 1, type: 'result', id: 'x', ok: null }).data).toBe(before);
  });
});
