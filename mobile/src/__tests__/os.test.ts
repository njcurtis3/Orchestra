import * as Notifications from 'expo-notifications';

import * as os from '../notify/os';

const mockRemove = jest.fn();
let mockLast: unknown = null;

jest.mock('expo-notifications', () => ({
  AndroidImportance: { HIGH: 4 },
  AndroidNotificationVisibility: { PRIVATE: 2 },
  setNotificationChannelAsync: jest.fn(async () => null),
  addNotificationResponseReceivedListener: jest.fn(() => ({ remove: (...a: unknown[]) => mockRemove(...a) })),
  getLastNotificationResponse: jest.fn(() => mockLast),
  clearLastNotificationResponse: jest.fn(() => {
    mockLast = null;
  }),
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

  it('a warm tap is cleared, so a remounted root does not replay it', () => {
    const cb = jest.fn();
    os.onTap(cb);
    const listener = (Notifications.addNotificationResponseReceivedListener as jest.Mock).mock.calls[0][0];
    mockLast = response({ id: 'i1' });
    listener(mockLast);
    expect(Notifications.clearLastNotificationResponse).toHaveBeenCalled();
    expect(os.takeLaunchTap()).toBeNull();
  });

  it('takeLaunchTap returns the data and clears it, or null with no tap', () => {
    expect(os.takeLaunchTap()).toBeNull();
    expect(Notifications.clearLastNotificationResponse).not.toHaveBeenCalled();
    mockLast = response({ id: 'i2' });
    expect(os.takeLaunchTap()).toEqual({ data: { id: 'i2' } });
    expect(Notifications.clearLastNotificationResponse).toHaveBeenCalled();
    expect(os.takeLaunchTap()).toBeNull();
  });

  it('tapRoute opens a valid id and sends bad data home', () => {
    expect(os.tapRoute({ id: 'i3', kind: 'question' })).toEqual({ pathname: '/inbox/[id]', params: { id: 'i3' } });
    expect(os.tapRoute({ id: '' })).toBeNull();
    expect(os.tapRoute({ id: 5 })).toBeNull();
    expect(os.tapRoute(null)).toBeNull();
    expect(os.tapRoute({ id: 'x'.repeat(129) })).toBeNull();
  });
});
