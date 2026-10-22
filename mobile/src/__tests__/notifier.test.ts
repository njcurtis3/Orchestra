import * as Notifications from 'expo-notifications';

import { createNotifier, isSuppressed } from '../notify/notifier';
import * as os from '../notify/os';
import { inboxItem, sessionOf } from './fake-host';
import type { InboxItem, SessionSummary } from '../../../protocol/types';

jest.mock('expo-notifications', () => ({
  AndroidImportance: { HIGH: 4 },
  AndroidNotificationVisibility: { PRIVATE: 2 },
  setNotificationChannelAsync: jest.fn(async () => null),
  requestPermissionsAsync: jest.fn(async () => ({ status: 'granted', canAskAgain: true })),
  getPermissionsAsync: jest.fn(async () => ({ status: 'undetermined', canAskAgain: true })),
  scheduleNotificationAsync: jest.fn(async () => 'id'),
  dismissNotificationAsync: jest.fn(async () => undefined),
  dismissAllNotificationsAsync: jest.fn(async () => undefined),
  setNotificationHandler: jest.fn(),
}));

const item = (id: string): InboxItem => inboxItem(id) as InboxItem;
const sessions = [sessionOf('s1')] as unknown as SessionSummary[];

function make(state: string) {
  const adapter = {
    post: jest.fn(async () => undefined),
    dismiss: jest.fn(async () => undefined),
    dismissAll: jest.fn(async () => undefined),
  };
  return { adapter, n: createNotifier(adapter, () => state) };
}

describe('isSuppressed', () => {
  it('is true only for active on home or active on the same item', () => {
    expect(isSuppressed('active', '/', 'i1')).toBe(true);
    expect(isSuppressed('active', '/inbox/i1', 'i1')).toBe(true);
    expect(isSuppressed('active', '/inbox/i2', 'i1')).toBe(false);
    expect(isSuppressed('active', '/pair', 'i1')).toBe(false);
    expect(isSuppressed('background', '/', 'i1')).toBe(false);
    expect(isSuppressed('inactive', '/inbox/i1', 'i1')).toBe(false);
  });
});

describe('notifier', () => {
  it('posts a fresh item when the app is in the background', () => {
    const { adapter, n } = make('background');
    n.onInbox({ fresh: [item('i1')], gone: [] }, sessions);
    expect(adapter.post).toHaveBeenCalledTimes(1);
    expect(adapter.post).toHaveBeenCalledWith(
      expect.objectContaining({ identifier: 'i1', content: expect.objectContaining({ title: 'Permission from Project' }) }),
    );
  });

  it('does not post while active on home, but does on another screen', () => {
    const { adapter, n } = make('active');
    n.onInbox({ fresh: [item('i1')], gone: [] }, sessions);
    expect(adapter.post).not.toHaveBeenCalled();
    n.setRoute('/pair');
    n.onInbox({ fresh: [item('i1')], gone: [] }, sessions);
    expect(adapter.post).toHaveBeenCalledTimes(1);
  });

  it('dismisses gone ids and calls dismissAll on a reset', () => {
    const { adapter, n } = make('background');
    n.onInbox({ fresh: [], gone: ['a', 'b'] }, sessions);
    expect(adapter.dismiss.mock.calls).toEqual([['a'], ['b']]);
    expect(adapter.dismissAll).not.toHaveBeenCalled();
    n.onInbox({ fresh: [], gone: [], reset: true }, sessions);
    expect(adapter.dismissAll).toHaveBeenCalledTimes(1);
  });

  it('survives an adapter that rejects', async () => {
    const { adapter, n } = make('background');
    adapter.post.mockRejectedValueOnce(new Error('boom'));
    expect(() => n.onInbox({ fresh: [item('i1')], gone: [] }, sessions)).not.toThrow();
    await Promise.resolve();
  });
});

describe('os adapter', () => {
  it('creates the channel before it requests permission, with a private lock screen', async () => {
    const order: string[] = [];
    (Notifications.setNotificationChannelAsync as jest.Mock).mockImplementationOnce(async () => {
      order.push('channel');
    });
    (Notifications.requestPermissionsAsync as jest.Mock).mockImplementationOnce(async () => {
      order.push('request');
      return { status: 'denied', canAskAgain: false };
    });
    const r = await os.request();
    expect(order).toEqual(['channel', 'request']);
    expect(r).toEqual({ status: 'denied', canAskAgain: false });
    expect(Notifications.setNotificationChannelAsync).toHaveBeenCalledWith(
      'needs-you',
      expect.objectContaining({ name: 'Needs you', importance: 4, lockscreenVisibility: 2 }),
    );
  });

  it('ensures the channel before it schedules a post', async () => {
    const order: string[] = [];
    (Notifications.setNotificationChannelAsync as jest.Mock).mockImplementationOnce(async () => {
      order.push('channel');
    });
    (Notifications.scheduleNotificationAsync as jest.Mock).mockImplementationOnce(async () => {
      order.push('schedule');
      return 'id';
    });
    await os.post({ identifier: 'i1', content: { title: 't', body: 'b', data: { id: 'i1', kind: 'permission' } } });
    expect(order).toEqual(['channel', 'schedule']);
  });

  it('posts on the channel with only id and kind as data', async () => {
    await os.post({ identifier: 'i1', content: { title: 't', body: 'b', data: { id: 'i1', kind: 'permission' } } });
    expect(Notifications.scheduleNotificationAsync).toHaveBeenCalledWith({
      identifier: 'i1',
      content: { title: 't', body: 'b', data: { id: 'i1', kind: 'permission' } },
      trigger: { channelId: 'needs-you' },
    });
  });
});
