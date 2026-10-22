import { BODY_FALLBACK, BODY_MAX, buildPayload, parseTap } from '../notify/payload';
import { inboxItem } from './fake-host';
import type { InboxItem } from '../../../protocol/types';

const secret = {
  tool_name: 'TOOLNAME-SECRET',
  tool_input_preview: 'rm -rf PREVIEW-SECRET',
  questions: [{ question: 'QUESTION-SECRET', header: 'h', multi_select: false, options: [] }],
  result_text: 'RESULT-SECRET',
} as unknown as Partial<InboxItem>;
const item = (over: Partial<InboxItem> = {}): InboxItem => ({ ...(inboxItem('i1') as InboxItem), ...over });

describe('buildPayload', () => {
  it('data keys are exactly id and kind', () => {
    const p = buildPayload(item(secret), 'Proj');
    expect(Object.keys(p.content.data).sort()).toEqual(['id', 'kind']);
    expect(p.content.data).toEqual({ id: 'i1', kind: 'permission' });
    expect(p.identifier).toBe('i1');
  });

  it('never carries the tool input, questions or result text', () => {
    const text = JSON.stringify(buildPayload(item(secret), 'Proj'));
    for (const s of ['PREVIEW-SECRET', 'QUESTION-SECRET', 'RESULT-SECRET', 'TOOLNAME-SECRET']) expect(text).not.toContain(s);
  });

  it('titles by kind and project, with a fallback project', () => {
    expect(buildPayload(item(), 'Proj').content.title).toBe('Permission from Proj');
    expect(buildPayload(item({ kind: 'question' }), 'Proj').content.title).toBe('Question from Proj');
    expect(buildPayload(item(), undefined).content.title).toBe('Permission from Unknown project');
    expect(buildPayload(item(), '  ').content.title).toBe('Permission from Unknown project');
  });

  it('cuts the body to 100 characters with an ellipsis', () => {
    const body = buildPayload(item({ summary: 'x'.repeat(500) }), 'P').content.body;
    expect(body.length).toBe(BODY_MAX);
    expect(body.endsWith('…')).toBe(true);
    expect(buildPayload(item({ summary: 'short' }), 'P').content.body).toBe('short');
  });

  it('does not split a surrogate pair when cutting', () => {
    const body = buildPayload(item({ summary: 'a'.repeat(98) + '\u{1F600}' + 'tail' }), 'P').content.body;
    expect(body).toBe('a'.repeat(98) + '\u{1F600}' + '…');
    expect(body).not.toMatch(/[\uD800-\uDBFF](?![\uDC00-\uDFFF])/);
    const long = buildPayload(item({ summary: 'a'.repeat(97) + '\u{1F600}' + 'tail' }), 'P').content.body;
    expect(Array.from(long).length).toBe(BODY_MAX);
  });

  it('falls back when the summary is empty', () => {
    expect(buildPayload(item({ summary: '   ' }), 'P').content.body).toBe(BODY_FALLBACK);
    expect(BODY_FALLBACK).toBe('Open Orchestra to answer.');
  });
});

describe('parseTap', () => {
  it('returns a good id', () => {
    expect(parseTap({ id: 'abc', kind: 'permission' })).toBe('abc');
    expect(parseTap({ id: 'a'.repeat(128) })).toBe('a'.repeat(128));
  });

  it('rejects a non-string, an empty string and more than 128 chars', () => {
    expect(parseTap({ id: 5 })).toBeNull();
    expect(parseTap({ id: '' })).toBeNull();
    expect(parseTap({ id: 'a'.repeat(129) })).toBeNull();
    expect(parseTap(null)).toBeNull();
    expect(parseTap('x')).toBeNull();
    expect(parseTap({})).toBeNull();
  });
});
