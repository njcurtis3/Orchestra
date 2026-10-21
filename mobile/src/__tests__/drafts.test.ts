import { act, renderHook } from '@testing-library/react-native';

import { clearDrafts, getDraft, useDraft } from '@/inbox/drafts';

beforeEach(() => clearDrafts());

describe('drafts', () => {
  it('starts blank', () => {
    expect(getDraft('x')).toEqual({ message: '', question: { picks: {}, other: {} } });
  });

  it('survives unmount and remount', async () => {
    const first = await renderHook(() => useDraft('i1'));
    await act(async () => first.result.current[1]((d) => ({ ...d, message: 'later' })));
    expect(first.result.current[0].message).toBe('later');
    await first.unmount();
    const second = await renderHook(() => useDraft('i1'));
    expect(second.result.current[0].message).toBe('later');
  });

  it('keeps items apart', async () => {
    const a = await renderHook(() => useDraft('a'));
    await act(async () => a.result.current[1]((d) => ({ ...d, message: 'for a' })));
    const b = await renderHook(() => useDraft('b'));
    expect(b.result.current[0].message).toBe('');
  });

  it('is forgotten by clearDrafts', async () => {
    const a = await renderHook(() => useDraft('a'));
    await act(async () => a.result.current[1]((d) => ({ ...d, message: 'x' })));
    clearDrafts();
    expect(getDraft('a').message).toBe('');
  });
});
