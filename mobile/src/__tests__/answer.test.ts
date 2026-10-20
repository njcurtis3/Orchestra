import type { InboxItem, Question } from '../../../protocol/types';
import {
  allowAnswer,
  byteLength,
  collect,
  denyAnswer,
  denyTooLong,
  draftFromAnswer,
  emptyQuestionDraft,
  isComplete,
  listedItems,
  MAX_ANSWER_BYTES,
  MAX_TEXT_BYTES,
  questionAnswer,
  questionLimit,
  setOther,
  togglePick,
} from '@/inbox/answer';
import type { Outbox } from '@/link/outbox';

const q = (question: string, multi = false, labels = ['A', 'B', 'C']): Question => ({
  question,
  header: null,
  multi_select: multi,
  options: labels.map((label) => ({ label, description: null })),
});

describe('permission answers', () => {
  it('Allow is the desktop shape, with no message', () => {
    expect(allowAnswer('i1')).toEqual({ kind: 'permission', item_id: 'i1', decision: 'allow' });
  });

  it('Deny carries the message as typed only when it has text, else none', () => {
    expect(denyAnswer('i1', '')).toEqual({ kind: 'permission', item_id: 'i1', decision: 'deny' });
    expect(denyAnswer('i1', '   ')).not.toHaveProperty('message');
    expect(denyAnswer('i1', ' no ')).toEqual({
      kind: 'permission',
      item_id: 'i1',
      decision: 'deny',
      message: ' no ',
    });
  });

  it('refuses a deny message over 32768 bytes and accepts exactly 32768', () => {
    expect(denyTooLong('a'.repeat(MAX_TEXT_BYTES))).toBe(false);
    expect(denyTooLong('a'.repeat(MAX_TEXT_BYTES + 1))).toBe(true);
    expect(denyTooLong('é'.repeat(16385))).toBe(true);
  });
});

describe('question answers', () => {
  it('counts UTF-8 bytes', () => {
    expect(byteLength('aé€😀')).toBe(1 + 2 + 3 + 4);
  });

  it('single select keeps one pick, keyed by question text', () => {
    const one = q('Which?');
    let d = togglePick(emptyQuestionDraft(), one, 'A', true);
    d = togglePick(d, one, 'B', true);
    expect(collect([one], d)).toEqual({ 'Which?': 'B' });
  });

  it('multi select and Other join with ", "', () => {
    const m = q('Pick', true);
    let d = togglePick(emptyQuestionDraft(), m, 'A', true);
    d = togglePick(d, m, 'C', true);
    d = setOther(d, m, '  mine  ');
    expect(collect([m], d)).toEqual({ Pick: 'A, C, mine' });
    d = togglePick(d, m, 'A', false);
    expect(collect([m], d)).toEqual({ Pick: 'C, mine' });
  });

  it('Other alone is an answer, blank Other is not', () => {
    const one = q('Which?');
    expect(collect([one], setOther(emptyQuestionDraft(), one, '  '))).toEqual({});
    expect(collect([one], setOther(emptyQuestionDraft(), one, 'x'))).toEqual({ 'Which?': 'x' });
  });

  it('is complete only when every question has an answer', () => {
    const a = q('One');
    const b = q('Two');
    let d = togglePick(emptyQuestionDraft(), a, 'A', true);
    expect(isComplete([a, b], collect([a, b], d))).toBe(false);
    d = setOther(d, b, 'y');
    expect(isComplete([a, b], collect([a, b], d))).toBe(true);
    expect(isComplete([], {})).toBe(false);
  });

  it('builds the wire shape', () => {
    expect(questionAnswer('i2', { Q: 'A' })).toEqual({
      kind: 'question',
      item_id: 'i2',
      answers: { Q: 'A' },
    });
  });

  it('refuses a 4097-byte value and accepts 4096', () => {
    const one = q('Which?');
    expect(questionLimit([one], { 'Which?': 'a'.repeat(MAX_ANSWER_BYTES) })).toBe('ok');
    expect(questionLimit([one], { 'Which?': 'a'.repeat(MAX_ANSWER_BYTES + 1) })).toBe('too_long');
  });

  it('refuses a 4097-byte question text', () => {
    const long = 'q'.repeat(MAX_ANSWER_BYTES + 1);
    expect(questionLimit([q(long)], { [long]: 'A' })).toBe('too_long');
  });

  it('refuses 33 questions and accepts 32', () => {
    const many = (n: number) => Array.from({ length: n }, (_, i) => q(`Q${i}`));
    const answers = (qs: Question[]) => Object.fromEntries(qs.map((x) => [x.question, 'A']));
    expect(questionLimit(many(32), answers(many(32)))).toBe('ok');
    expect(questionLimit(many(33), answers(many(33)))).toBe('too_many');
  });

  it('rebuilds a draft from a held answer', () => {
    const s = q('Which?');
    const m = q('Pick', true, ['A', 'B, C']);
    const d = draftFromAnswer([s, m], { 'Which?': 'C', Pick: 'A, B, C, my note' });
    expect(d.picks['Which?']).toEqual(['C']);
    expect(d.picks.Pick).toEqual(['A', 'B, C']);
    expect(d.other.Pick).toBe('my note');
    expect(collect([s, m], d)['Which?']).toBe('C');
  });
});

describe('listedItems', () => {
  const item = (id: string, over: Partial<InboxItem> = {}): InboxItem => ({
    id,
    session_id: 's',
    kind: 'permission',
    created_at: 1,
    resolved: false,
    expired: false,
    summary: '',
    tool_name: null,
    tool_input_preview: null,
    questions: null,
    result_text: null,
    ...over,
  });
  const entry = (id: string, status: string) =>
    ({ item_id: id, status, call_id: 'c', attempt: 1, answer: allowAnswer(id) }) as never;

  it('lists pending permission and question items, newest first', () => {
    const inbox = [
      item('old', { created_at: 1 }),
      item('new', { kind: 'question', created_at: 9 }),
      item('done', { kind: 'finished' }),
      item('res', { resolved: true }),
    ];
    expect(listedItems(inbox, {}).map((i) => i.id)).toEqual(['new', 'old']);
  });

  it('keeps a not-sent outcome listed, drops sent', () => {
    const inbox = [item('a', { resolved: true }), item('b', { resolved: true })];
    const outbox: Outbox = { a: entry('a', 'answered'), b: entry('b', 'sent') };
    expect(listedItems(inbox, outbox).map((i) => i.id)).toEqual(['a']);
  });
});
