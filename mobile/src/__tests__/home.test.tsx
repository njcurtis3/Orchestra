import { fireEvent, render, screen } from '@testing-library/react-native';
import { SafeAreaProvider } from 'react-native-safe-area-context';

import HomeScreen from '@/app/index';
import { copy, extra, fill, splitActions } from '@/constants/copy';
import type { LinkContextValue } from '@/link/provider';

const mockRouter = { push: jest.fn(), replace: jest.fn(), back: jest.fn() };
let mockLink: LinkContextValue;

jest.mock('expo-router', () => ({ useRouter: () => mockRouter }));
jest.mock('@/link/provider', () => ({ useLink: () => mockLink }));

const metrics = {
  frame: { x: 0, y: 0, width: 360, height: 800 },
  insets: { top: 24, left: 0, right: 0, bottom: 16 },
};

const session = (id: string, over: Record<string, unknown> = {}) => ({
  id,
  project_id: `p-${id}`,
  project_name: `Project ${id}`,
  status: 'working',
  started_at: 1,
  last_activity_at: 10,
  last_message: `message ${id}`,
  claude_session_id: null,
  ...over,
});

const host = { host_id: 'a'.repeat(32), host_name: 'Desk', fingerprint: 'f', addrs: ['10.0.0.2:47800'] };

function link(over: Partial<LinkContextValue> = {}): LinkContextValue {
  return {
    phase: 'live',
    host,
    sessions: [],
    inbox: [],
    outbox: {},
    inboxCount: 0,
    canAnswer: true,
    error: null,
    scopes: ['read', 'answer'],
    pairStep: null,
    pairFingerprint: null,
    retryAt: null,
    pair: jest.fn(),
    cancelPair: jest.fn(),
    retryNow: jest.fn(),
    forget: jest.fn().mockResolvedValue(undefined),
    answer: jest.fn(),
    cancelAnswer: jest.fn(),
    retryAnswer: jest.fn(),
    dismissAnswer: jest.fn(),
    ...over,
  } as LinkContextValue;
}

async function show(over: Partial<LinkContextValue> = {}) {
  mockLink = link(over);
  await render(
    <SafeAreaProvider initialMetrics={metrics}>
      <HomeScreen />
    </SafeAreaProvider>,
  );
}

const text = (template: string, values: Record<string, string | number> = { host: 'Desk' }) =>
  fill(splitActions(template).text, values);

jest.setTimeout(30_000);

beforeEach(() => jest.clearAllMocks());

describe('HomeScreen not paired', () => {
  it('shows the title, body and the Pair button, and the old placeholder is gone', async () => {
    await show({ phase: 'unpaired', host: null, scopes: [] });
    expect(screen.getByText(copy.not_paired_title)).toBeTruthy();
    expect(screen.getByText(copy.not_paired_body)).toBeTruthy();
    expect(screen.getByText(copy.not_paired_hint)).toBeTruthy();
    expect(screen.queryByText(/later version/)).toBeNull();
    await fireEvent.press(screen.getByRole('button', { name: copy.not_paired_button }));
    expect(mockRouter.push).toHaveBeenCalledWith('/pair');
  });

  it('renders a blank screen while loading', async () => {
    await show({ phase: 'loading', host: null });
    expect(screen.queryByText(copy.not_paired_title)).toBeNull();
  });

  it('shows the removed banner above the not-paired view', async () => {
    await show({ phase: 'unpaired', host: null, error: { kind: 'Revoked' } });
    expect(screen.getByText(text(copy.revoked, { host: 'your desktop' }))).toBeTruthy();
    expect(screen.getByText(copy.not_paired_title)).toBeTruthy();
  });
});

describe('HomeScreen connected', () => {
  it('shows the host, the need-you count and two session rows', async () => {
    await show({
      sessions: [
        session('1', { last_activity_at: 5 }),
        session('2', { status: 'waiting_permission', last_activity_at: 9 }),
      ] as never,
      inboxCount: 2,
    });
    expect(screen.getByText('Desk')).toBeTruthy();
    expect(screen.getByText(extra.need_you(2))).toBeTruthy();
    expect(screen.getByText('Project 1')).toBeTruthy();
    expect(screen.getByText('Project 2')).toBeTruthy();
    expect(screen.getByText('Needs permission')).toBeTruthy();
    expect(screen.getByText('message 1')).toBeTruthy();
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('shows the empty state when live with no sessions', async () => {
    await show();
    expect(screen.getByText(text(copy.no_sessions))).toBeTruthy();
  });
});

describe('HomeScreen banners', () => {
  it('offline, over the cached list', async () => {
    await show({ phase: 'offline', sessions: [session('1')] as never });
    expect(screen.getByText(copy.offline)).toBeTruthy();
    expect(screen.getByText('Project 1')).toBeTruthy();
  });

  it('reconnecting, over the cached list', async () => {
    await show({ phase: 'connecting', sessions: [session('1')] as never });
    expect(screen.getByText(text(copy.reconnecting))).toBeTruthy();
    expect(screen.getByText('Project 1')).toBeTruthy();
  });

  it('unreachable shows the countdown and Try now calls retryNow', async () => {
    const retryNow = jest.fn();
    await show({ phase: 'waiting', retryAt: Date.now() + 12_000, retryNow });
    expect(screen.getByText(/Trying again in (11|12) s\./)).toBeTruthy();
    await fireEvent.press(screen.getByRole('button', { name: 'Try now' }));
    expect(retryNow).toHaveBeenCalled();
  });

  it('version mismatch names the supported list and the version this app speaks', async () => {
    await show({ phase: 'waiting', error: { kind: 'VersionMismatch', supported: [2, 3] } });
    expect(
      screen.getByText(text(copy.version_mismatch, { host: 'Desk', supported: '2, 3' })),
    ).toBeTruthy();
    expect(screen.getByText(/version 2, 3; this app speaks version 1\./)).toBeTruthy();
  });

  it('not paired with this host offers Pair again and Forget', async () => {
    const forget = jest.fn().mockResolvedValue(undefined);
    await show({ phase: 'not_paired', error: { kind: 'LinkRefused' }, forget });
    expect(screen.getByText(text(copy.not_paired_with_host))).toBeTruthy();
    await fireEvent.press(screen.getByRole('button', { name: 'Pair again' }));
    expect(mockRouter.push).toHaveBeenCalledWith('/pair');
    await fireEvent.press(screen.getByRole('button', { name: 'Forget this desktop' }));
    expect(forget).toHaveBeenCalled();
  });

  it('no read scope', async () => {
    await show({ scopes: ['answer'] });
    expect(screen.getByText(text(copy.no_read_scope))).toBeTruthy();
    expect(screen.queryByText(text(copy.no_sessions))).toBeNull();
  });
});

describe('HomeScreen needs you', () => {
  const inboxItem = (id: string, session_id: string, over: Record<string, unknown> = {}) => ({
    id,
    session_id,
    kind: 'permission',
    created_at: 1,
    resolved: false,
    expired: false,
    summary: `summary ${id}`,
    tool_name: null,
    tool_input_preview: null,
    questions: null,
    result_text: null,
    ...over,
  });
  const two = [session('1'), session('2')] as never;

  it('says nothing needs you when the list is empty', async () => {
    await show({ sessions: two });
    expect(screen.getByText(copy.inbox_title)).toBeTruthy();
    expect(screen.getByText(copy.inbox_empty)).toBeTruthy();
  });

  it("lists two sessions' items, newest first, and opens one", async () => {
    await show({
      sessions: two,
      inbox: [
        inboxItem('old', '1', { created_at: 1 }),
        inboxItem('new', '2', { kind: 'question', created_at: 9 }),
        inboxItem('fin', '1', { kind: 'finished', created_at: 20 }),
      ] as never,
      inboxCount: 2,
    });
    expect(screen.queryByText(copy.inbox_empty)).toBeNull();
    const rows = screen.getAllByRole('button').map((b) => b.props.accessibilityLabel);
    expect(rows).toEqual(['Question from Project 2, Waiting for you', 'Permission from Project 1, Waiting for you']);
    expect(screen.queryByText('summary fin')).toBeNull();
    await fireEvent.press(screen.getByRole('button', { name: /Question from Project 2/ }));
    expect(mockRouter.push).toHaveBeenCalledWith({ pathname: '/inbox/[id]', params: { id: 'new' } });
  });

  it.each(['gone', 'answered', 'expired'])(
    'lists a %s outcome whose item left the inbox until dismissed',
    async (status) => {
      const held = { item_id: 'x', status, call_id: 'c', attempt: 1, answer: { kind: 'permission' } };
      await show({ sessions: two, inbox: [], outbox: { x: held } as never });
      expect(screen.getAllByRole('button')).toHaveLength(1);
      expect(
        screen.getByRole('button', { name: /^Permission from Unknown project, /i }),
      ).toBeTruthy();
      await show({ sessions: two, inbox: [], outbox: {} });
      expect(screen.queryByRole('button', { name: /^Permission from/ })).toBeNull();
    },
  );

  it('keeps a not-sent outcome listed until dismissed', async () => {
    const answered = { item_id: 'a', status: 'answered', call_id: 'c', attempt: 1, answer: {} };
    const inbox = [inboxItem('a', '1', { resolved: true })] as never;
    await show({ sessions: two, inbox, outbox: { a: answered } as never });
    expect(
      screen.getByRole('button', { name: 'Permission from Project 1, Already answered' }),
    ).toBeTruthy();
    await show({ sessions: two, inbox, outbox: {} });
    expect(screen.queryByRole('button', { name: /Permission from Project 1/ })).toBeNull();
  });
});
