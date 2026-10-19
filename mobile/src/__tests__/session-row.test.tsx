import { render, screen } from '@testing-library/react-native';

import { SessionRow } from '@/components/session-row';
import { statusLabel } from '@/constants/copy';

const base = {
  id: 's',
  project_id: 'p',
  project_name: 'Alpha',
  status: 'working',
  started_at: 1,
  last_activity_at: 2,
  last_message: 'hello there',
  claude_session_id: null,
};

describe('SessionRow', () => {
  it.each([
    ['starting', 'Starting'],
    ['working', 'Working'],
    ['waiting_permission', 'Needs permission'],
    ['waiting_question', 'Has a question'],
    ['needs_attention', 'Needs you'],
    ['idle', 'Idle'],
    ['suspended', 'Suspended'],
    ['ended', 'Ended'],
    ['failed', 'Failed'],
  ])('labels %s as %s', async (status, label) => {
    await render(<SessionRow session={{ ...base, status } as never} />);
    expect(screen.getByText(label)).toBeTruthy();
    expect(screen.getByText('Alpha')).toBeTruthy();
  });

  it('shows an unknown status as sent', async () => {
    await render(<SessionRow session={{ ...base, status: 'compacting' } as never} />);
    expect(screen.getByText('compacting')).toBeTruthy();
    expect(statusLabel('constructor')).toBe('constructor');
  });

  it('keeps the last message to one line', async () => {
    await render(<SessionRow session={base as never} />);
    expect(screen.getByText('hello there').props.numberOfLines).toBe(1);
  });

  it('omits the message line when there is none', async () => {
    await render(<SessionRow session={{ ...base, last_message: null } as never} />);
    expect(screen.queryByText('hello there')).toBeNull();
  });
});
