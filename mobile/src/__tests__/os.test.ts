import * as Notifications from 'expo-notifications';

import * as os from '../notify/os';

const mockRemove = jest.fn();
let mockLast: unknown = null;

jest.mock('expo-notifications', () => ({
  AndroidImportance: { HIGH: 4 },
  AndroidNotificationVisibility: { PRIVATE: 2 },
  setNotificationChannelAsync: jest.fn(async () => null),
  addNotificationResponseReceivedListener: jest.fn(() => ({ remove: (...a: unknown[]) => mockRemove(...a) })),
  getLastNotificationResponseAsync: jest.fn(async () => mockLast),
  clearLastNotificationResponseAsync: jest.fn(async () => undefined),
}));

const response = (data: unknown) => ({ notification: { request: { content: { data } } } });

beforeEach(() => {
  jest.clearAllMocks();
  mockLast = null;
});

describe('os adapter', () => {
  it('creates the channel with no badge and a private lock screen', async () => {
    await os.ensureChannel();
    expect(Notifications.setNotificationChannelAsync).toHaveBeenCalledWith(
      'needs-you',
      expect.objectContaining({ showBadge: false, lockscreenVisibility: 2 }),
    );
  });

  it('onTap passes the data to the callback and unsubscribes', () => {
    const cb = jest.fn();
    const off = os.onTap(cb);
    const listener = (Notifications.addNotificationResponseReceivedListener as jest.Mock).mock.calls[0][0];
    listener(response({ id: 'i1', kind: 'permission' }));
    expect(cb).toHaveBeenCalledWith({ id: 'i1', kind: 'permission' });
    off();
    expect(mockRemove).toHaveBeenCalled();
  });

  it('takeLaunchTap returns the data and clears it, or null with no tap', async () => {
    expect(await os.takeLaunchTap()).toBeNull();
    expect(Notifications.clearLastNotificationResponseAsync).not.toHaveBeenCalled();
    mockLast = response({ id: 'i2' });
    expect(await os.takeLaunchTap()).toEqual({ data: { id: 'i2' } });
    expect(Notifications.clearLastNotificationResponseAsync).toHaveBeenCalled();
  });
});
