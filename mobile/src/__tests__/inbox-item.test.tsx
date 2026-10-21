import { fireEvent, render, screen } from '@testing-library/react-native';
import { SafeAreaProvider } from 'react-native-safe-area-context';

import InboxItemScreen from '@/app/inbox/[id]';
import { copy, fill, splitActions } from '@/constants/copy';
import { clearDrafts } from '@/inbox/drafts';
import type { LinkContextValue } from '@/link/provider';

const mockRouter = { push: jest.fn(), replace: jest.fn(), back: jest.fn(), canGoBack: jest.fn(() => true) };
let mockLink: LinkContextValue;
let mockId = 'i1';

jest.mock('expo-router', () => ({
  useRouter: () => mockRouter,
  useLocalSearchParams: () => ({ id: mockId }),
}));
jest.mock('@/link/provider', () => ({ useLink: () => mockLink }));

const metrics = {
  frame: { x: 0, y: 0, width: 360, height: 800 },
  insets: { top: 24, left: 0, right: 0, bottom: 16 },
};

const host = { host_id: 'a'.repeat(32), host_name: 'Desk', fingerprint: 'f', addrs: ['10.0.0.2:47800'] };

const permission = (over: Record<string, unknown> = {}) => ({
  id: 'i1',
  session_id: 's1',
  kind: 'permission',
  created_at: 5,
  resolved: false,
  expired: false,
  summary: 'Run a command',
  tool_name: 'Bash',
  tool_input_preview: 'rm -rf build',
  questions: null,
  result_text: null,
  ...over,
});

const question = (text: string, over: Record<string, unknown> = {}) => ({
  question: text,
  header: 'Setup',
  multi_select: false,
  options: [
    { label: 'Red', description: 'warm' },
    { label: 'Blue', description: null },
  ],
  ...over,
});

const asking = (questions: unknown[], over: Record<string, unknown> = {}) =>
  permission({ kind: 'question', tool_name: null, tool_input_preview: null, questions, ...over });

function link(over: Partial<LinkContextValue> = {}): LinkContextValue {
  return {
    phase: 'live',
    host,
    sessions: [{ id: 's1', project_name: 'Alpha', status: 'waiting_permission' }] as never,
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

const tree = (
  <SafeAreaProvider initialMetrics={metrics}>
    <InboxItemScreen />
  </SafeAreaProvider>
);

async function show(over: Partial<LinkContextValue> = {}) {
  mockLink = link(over);
  return render(tree);
}

const plain = (template: string, values: Record<string, string> = { host: 'Desk' }) =>
  fill(splitActions(template).text, values);

const entry = (status: string, answer: unknown, over: Record<string, unknown> = {}) => ({
  i1: { item_id: 'i1', answer, call_id: 'a-1', attempt: 1, status, ...over },
});

// The live region is on an ancestor of the status text.
function liveRegion(node: { parent: unknown; props: Record<string, unknown> } | null): unknown {
  for (let n = node; n; n = n.parent as typeof node) {
    if (n.props?.accessibilityLiveRegion) return n.props.accessibilityLiveRegion;
  }
  return undefined;
}

const allow = { kind: 'permission', item_id: 'i1', decision: 'allow' };

jest.setTimeout(30_000);

beforeEach(() => {
  jest.clearAllMocks();
  clearDrafts();
  mockId = 'i1';
});

describe('permission item', () => {
  it('shows kind, project, summary, tool and the input preview', async () => {
    await show({ inbox: [permission()] as never });
    expect(screen.getByText(copy.kind_permission)).toBeTruthy();
    expect(screen.getByText('Alpha')).toBeTruthy();
    expect(screen.getByText('Run a command')).toBeTruthy();
    expect(screen.getByText('Tool: Bash')).toBeTruthy();
    const preview = screen.getByText('rm -rf build');
    expect(preview.props.selectable).toBe(true);
  });

  it('Allow sends the desktop shape', async () => {
    await show({ inbox: [permission()] as never });
    await fireEvent.press(screen.getByRole('button', { name: 'Allow Bash' }));
    expect(mockLink.answer).toHaveBeenCalledWith(allow);
  });

  it('Deny sends no message when the box is blank', async () => {
    await show({ inbox: [permission()] as never });
    await fireEvent.changeText(screen.getByLabelText(copy.deny_message), '   ');
    await fireEvent.press(screen.getByRole('button', { name: 'Deny Bash' }));
    expect(mockLink.answer).toHaveBeenCalledWith({ kind: 'permission', item_id: 'i1', decision: 'deny' });
  });

  it('Deny sends the message as typed', async () => {
    await show({ inbox: [permission()] as never });
    await fireEvent.changeText(screen.getByLabelText(copy.deny_message), 'not now');
    await fireEvent.press(screen.getByRole('button', { name: 'Deny Bash' }));
    expect(mockLink.answer).toHaveBeenCalledWith({
      kind: 'permission',
      item_id: 'i1',
      decision: 'deny',
      message: 'not now',
    });
  });

  it('refuses a 32769-byte deny message, with copy, and keeps the text', async () => {
    await show({ inbox: [permission()] as never });
    const box = screen.getByLabelText(copy.deny_message);
    await fireEvent.changeText(box, 'a'.repeat(32769));
    expect(screen.getByText(copy.deny_too_long)).toBeTruthy();
    await fireEvent.press(screen.getByRole('button', { name: 'Deny Bash' }));
    expect(mockLink.answer).not.toHaveBeenCalled();
    expect(screen.getByLabelText(copy.deny_message).props.value).toHaveLength(32769);
  });

  it('labels the buttons without a tool name', async () => {
    await show({ inbox: [permission({ tool_name: null })] as never });
    expect(screen.getByRole('button', { name: 'Allow' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Deny' })).toBeTruthy();
  });
});

describe('question item', () => {
  it('single select, multi select and Other join with ", " keyed by question text', async () => {
    const qs = [
      question('Colour?'),
      question('Extras?', { multi_select: true, options: [{ label: 'A', description: null }, { label: 'B', description: null }] }),
    ];
    await show({ inbox: [asking(qs)] as never });
    await fireEvent.press(screen.getByRole('radio', { name: 'Red. warm' }));
    await fireEvent.press(screen.getByRole('radio', { name: 'Blue' }));
    await fireEvent.press(screen.getByRole('checkbox', { name: 'A' }));
    await fireEvent.press(screen.getByRole('checkbox', { name: 'B' }));
    await fireEvent.changeText(screen.getByLabelText('Other: Extras?'), ' note ');
    await fireEvent.press(screen.getByRole('button', { name: copy.send_answers }));
    expect(mockLink.answer).toHaveBeenCalledWith({
      kind: 'question',
      item_id: 'i1',
      answers: { 'Colour?': 'Blue', 'Extras?': 'A, B, note' },
    });
  });

  it('Send is disabled until every question has an answer', async () => {
    const qs = [question('One?'), question('Two?')];
    await show({ inbox: [asking(qs)] as never });
    const send = () => screen.getByRole('button', { name: copy.send_answers });
    expect(send().props.accessibilityState.disabled).toBe(true);
    await fireEvent.press(screen.getAllByRole('radio', { name: 'Blue' })[0]);
    expect(send().props.accessibilityState.disabled).toBe(true);
    await fireEvent.changeText(screen.getByLabelText('Other: Two?'), 'x');
    expect(send().props.accessibilityState.disabled).toBe(false);
  });

  it('radio and checkbox rows carry accessibilityState checked', async () => {
    const qs = [
      question('One?'),
      question('Two?', { multi_select: true }),
    ];
    await show({ inbox: [asking(qs)] as never });
    const radio = () => screen.getByRole('radio', { name: 'Red. warm' });
    expect(radio().props.accessibilityState.checked).toBe(false);
    await fireEvent.press(radio());
    expect(radio().props.accessibilityState.checked).toBe(true);
    const box = () => screen.getByRole('checkbox', { name: 'Blue' });
    expect(box().props.accessibilityState.checked).toBe(false);
    await fireEvent.press(box());
    expect(box().props.accessibilityState.checked).toBe(true);
    await fireEvent.press(box());
    expect(box().props.accessibilityState.checked).toBe(false);
  });

  it('the question text and its header are both headers', async () => {
    await show({ inbox: [asking([question('Colour?')])] as never });
    expect(screen.getByText('Setup').props.accessibilityRole).toBe('header');
    expect(screen.getByText('Colour?').props.accessibilityRole).toBe('header');
  });

  it('refuses a 4097-byte value, with copy, and keeps the text', async () => {
    await show({ inbox: [asking([question('Colour?')])] as never });
    await fireEvent.changeText(screen.getByLabelText('Other: Colour?'), 'a'.repeat(4097));
    expect(screen.getByText(copy.answer_too_long)).toBeTruthy();
    await fireEvent.press(screen.getByRole('button', { name: copy.send_answers }));
    expect(mockLink.answer).not.toHaveBeenCalled();
    expect(screen.getByLabelText('Other: Colour?').props.value).toHaveLength(4097);
  });

  it('refuses a 4097-byte question text', async () => {
    const long = 'q'.repeat(4097);
    await show({ inbox: [asking([question(long)])] as never });
    await fireEvent.press(screen.getByRole('radio', { name: 'Blue' }));
    expect(screen.getByText(copy.answer_too_long)).toBeTruthy();
    await fireEvent.press(screen.getByRole('button', { name: copy.send_answers }));
    expect(mockLink.answer).not.toHaveBeenCalled();
  });

  it('refuses 33 questions, with copy', async () => {
    const qs = Array.from({ length: 33 }, (_, i) => question(`Q${i}`));
    await show({ inbox: [asking(qs)] as never });
    for (const r of screen.getAllByRole('radio', { name: 'Blue' })) await fireEvent.press(r);
    expect(screen.getByText(copy.too_many_questions)).toBeTruthy();
    await fireEvent.press(screen.getByRole('button', { name: copy.send_answers }));
    expect(mockLink.answer).not.toHaveBeenCalled();
  });

  it('sends 32 questions', async () => {
    const qs = Array.from({ length: 32 }, (_, i) => question(`Q${i}`));
    await show({ inbox: [asking(qs)] as never });
    for (const r of screen.getAllByRole('radio', { name: 'Blue' })) await fireEvent.press(r);
    await fireEvent.press(screen.getByRole('button', { name: copy.send_answers }));
    expect(mockLink.answer).toHaveBeenCalledTimes(1);
  });

  it('an empty questions list shows the no-questions copy and no Send', async () => {
    await show({ inbox: [asking([])] as never });
    expect(screen.getByText(copy.no_questions)).toBeTruthy();
    expect(screen.queryByRole('button', { name: copy.send_answers })).toBeNull();
  });
});

describe('status line', () => {
  const statuses: [string, string, string[]][] = [
    ['queued', copy.queued, ['Cancel']],
    ['sending', copy.sending, []],
    ['unconfirmed', copy.unconfirmed, ['Try again', 'Cancel']],
    ['sent', copy.sent, []],
    ['failed', copy.failed, ['Try again', 'Cancel']],
    ['refused', copy.no_answer_scope, ['Dismiss']],
    ['answered', copy.answered, ['Dismiss']],
    ['expired', copy.expired, ['Dismiss']],
    ['gone', copy.gone, ['Dismiss']],
  ];

  it.each(statuses)('%s renders its copy and actions', async (status, template, actions) => {
    await show({
      inbox: [permission()] as never,
      outbox: entry(status, allow, { message: 'host said no' }) as never,
    });
    const text = fill(splitActions(template).text, { host: 'Desk', reason: 'host said no' });
    expect(screen.getByText(text)).toBeTruthy();
    expect(liveRegion(screen.getByText(text))).toBe('polite');
    for (const label of ['Cancel', 'Try again', 'Dismiss']) {
      const has = screen.queryByRole('button', { name: label }) !== null;
      expect(has).toBe(actions.includes(label));
    }
  });

  it('the actions call the provider', async () => {
    const queued = await show({ inbox: [permission()] as never, outbox: entry('queued', allow) as never });
    await fireEvent.press(screen.getByRole('button', { name: 'Cancel' }));
    expect(mockLink.cancelAnswer).toHaveBeenCalledWith('i1');
    await queued.unmount();

    const failed = await show({ inbox: [permission()] as never, outbox: entry('failed', allow) as never });
    await fireEvent.press(screen.getByRole('button', { name: 'Try again' }));
    expect(mockLink.retryAnswer).toHaveBeenCalledWith('i1');
    await failed.unmount();

    await show({ inbox: [permission({ resolved: true })] as never, outbox: entry('answered', allow) as never });
    await fireEvent.press(screen.getByRole('button', { name: 'Dismiss' }));
    expect(mockLink.dismissAnswer).toHaveBeenCalledWith('i1');
  });

  it('a failed question answer rehydrates the controls and keeps the text', async () => {
    const answers = { 'Colour?': 'Red, painted' };
    await show({
      inbox: [asking([question('Colour?')])] as never,
      outbox: entry('failed', { kind: 'question', item_id: 'i1', answers }) as never,
    });
    expect(screen.getByRole('radio', { name: 'Red. warm' }).props.accessibilityState.checked).toBe(true);
    expect(screen.getByLabelText('Other: Colour?').props.value).toBe('painted');
  });

  it('a queued deny shows its message', async () => {
    const deny = { kind: 'permission', item_id: 'i1', decision: 'deny', message: 'wait' };
    await show({ inbox: [permission()] as never, outbox: entry('queued', deny) as never });
    expect(screen.getByLabelText(copy.deny_message).props.value).toBe('wait');
  });
});

describe('what hides the controls', () => {
  it('no answer scope shows the no-scope copy', async () => {
    await show({ inbox: [permission()] as never, canAnswer: false, scopes: ['read'] });
    expect(screen.getByText(fill(copy.no_answer_scope, { host: 'Desk' }))).toBeTruthy();
    expect(screen.queryByRole('button', { name: /Allow/ })).toBeNull();
    expect(screen.queryByLabelText(copy.deny_message)).toBeNull();
  });

  it('an expired item hides them', async () => {
    await show({ inbox: [permission({ expired: true })] as never });
    expect(screen.getByText(plain(copy.expired))).toBeTruthy();
    expect(screen.queryByRole('button', { name: /Allow/ })).toBeNull();
  });

  it('a resolved item hides them', async () => {
    await show({ inbox: [asking([question('Colour?')], { resolved: true })] as never });
    expect(screen.getByText(plain(copy.answered))).toBeTruthy();
    expect(screen.queryByRole('button', { name: copy.send_answers })).toBeNull();
  });

  it('a gone outcome for an item the host no longer holds shows its status and Dismiss', async () => {
    await show({ inbox: [], outbox: entry('gone', allow) as never });
    expect(screen.getByText(plain(copy.gone))).toBeTruthy();
    await fireEvent.press(screen.getByRole('button', { name: 'Dismiss' }));
    expect(mockLink.dismissAnswer).toHaveBeenCalledWith('i1');
  });

  it('a refused answer without the answer scope says so once', async () => {
    await show({
      inbox: [permission()] as never,
      canAnswer: false,
      scopes: ['read'],
      outbox: entry('refused', allow) as never,
    });
    expect(screen.getAllByText(plain(copy.no_answer_scope))).toHaveLength(1);
  });

  it('an unknown id shows the gone copy with Back', async () => {
    mockId = 'nope';
    await show({ inbox: [permission()] as never });
    expect(screen.getByText(plain(copy.gone))).toBeTruthy();
    await fireEvent.press(screen.getByRole('button', { name: copy.back }));
    expect(mockRouter.back).toHaveBeenCalled();
  });
});

describe('drafts', () => {
  it('a draft survives unmount and remount', async () => {
    const qs = [question('Colour?')];
    mockLink = link({ inbox: [asking(qs)] as never });
    const first = await render(tree);
    await fireEvent.press(screen.getByRole('radio', { name: 'Blue' }));
    await fireEvent.changeText(screen.getByLabelText('Other: Colour?'), 'teal');
    await first.unmount();
    await render(tree);
    expect(screen.getByRole('radio', { name: 'Blue' }).props.accessibilityState.checked).toBe(true);
    expect(screen.getByLabelText('Other: Colour?').props.value).toBe('teal');
  });

  it('a deny message survives remount', async () => {
    mockLink = link({ inbox: [permission()] as never });
    const first = await render(tree);
    await fireEvent.changeText(screen.getByLabelText(copy.deny_message), 'draft text');
    await first.unmount();
    await render(tree);
    expect(screen.getByLabelText(copy.deny_message).props.value).toBe('draft text');
  });

  it('Back with nothing behind it goes home', async () => {
    mockRouter.canGoBack.mockReturnValueOnce(false);
    await show({ inbox: [permission()] as never });
    await fireEvent.press(screen.getByRole('button', { name: copy.back }));
    expect(mockRouter.back).not.toHaveBeenCalled();
    expect(mockRouter.replace).toHaveBeenCalledWith('/');
  });

  it('Back is on screen and pops', async () => {
    await show({ inbox: [permission()] as never });
    await fireEvent.press(screen.getByRole('button', { name: copy.back }));
    expect(mockRouter.back).toHaveBeenCalled();
  });
});
