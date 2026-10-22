import { act, fireEvent, render, screen } from '@testing-library/react-native';
import { Linking, StyleSheet } from 'react-native';

import { AlertsCard, resetAlertsHidden } from '@/components/alerts-card';
import { copy, splitActions } from '@/constants/copy';
import { MinTouchTarget } from '@/constants/theme';
import * as os from '@/notify/os';

jest.mock('@/notify/os', () => ({ getStatus: jest.fn(), request: jest.fn() }));

const getStatus = os.getStatus as jest.Mock;
const request = os.request as jest.Mock;
const openSettings = jest.spyOn(Linking, 'openSettings').mockResolvedValue(undefined);

const show = async (s: { status: string; canAskAgain: boolean }) => {
  getStatus.mockResolvedValue(s);
  await render(<AlertsCard />);
  await act(async () => {});
};

beforeEach(() => {
  jest.clearAllMocks();
  resetAlertsHidden();
});

describe('AlertsCard', () => {
  it('asks nothing until Turn on alerts is pressed', async () => {
    await show({ status: 'undetermined', canAskAgain: true });
    expect(screen.getByText(copy.alerts_title)).toBeTruthy();
    expect(screen.getByText(copy.alerts_body)).toBeTruthy();
    expect(request).not.toHaveBeenCalled();
    request.mockResolvedValue({ status: 'granted', canAskAgain: true });
    await fireEvent.press(screen.getByRole('button', { name: copy.alerts_button }));
    expect(request).toHaveBeenCalledTimes(1);
    expect(await screen.findByText(splitActions(copy.alerts_on).text)).toBeTruthy();
  });

  it('denied with canAskAgain offers Try again, which requests again', async () => {
    await show({ status: 'denied', canAskAgain: true });
    expect(screen.getByText(splitActions(copy.alerts_denied_retry).text)).toBeTruthy();
    request.mockResolvedValue({ status: 'denied', canAskAgain: true });
    await fireEvent.press(screen.getByRole('button', { name: 'Try again' }));
    expect(request).toHaveBeenCalledTimes(1);
  });

  it('denied for good offers Open settings', async () => {
    await show({ status: 'denied', canAskAgain: false });
    expect(screen.getByText(splitActions(copy.alerts_denied_settings).text)).toBeTruthy();
    await fireEvent.press(screen.getByRole('button', { name: 'Open settings' }));
    expect(openSettings).toHaveBeenCalled();
    expect(request).not.toHaveBeenCalled();
  });

  it('granted shows the on line and Notification settings', async () => {
    await show({ status: 'granted', canAskAgain: true });
    expect(screen.getByText(splitActions(copy.alerts_on).text)).toBeTruthy();
    await fireEvent.press(screen.getByRole('button', { name: 'Notification settings' }));
    expect(openSettings).toHaveBeenCalled();
  });

  it('Not now hides the card', async () => {
    await show({ status: 'undetermined', canAskAgain: true });
    await fireEvent.press(screen.getByRole('button', { name: copy.alerts_not_now }));
    expect(screen.queryByText(copy.alerts_title)).toBeNull();
    expect(request).not.toHaveBeenCalled();
  });

  it('every button has a role, a label and a 48dp target', async () => {
    await show({ status: 'undetermined', canAskAgain: true });
    const buttons = screen.getAllByRole('button');
    expect(buttons).toHaveLength(2);
    for (const b of buttons) {
      expect(b.props.accessibilityRole).toBe('button');
      expect(b.props.accessibilityLabel).toBeTruthy();
      const style = StyleSheet.flatten(b.props.style);
      expect(Number(style.minHeight)).toBeGreaterThanOrEqual(MinTouchTarget);
    }
  });
});
