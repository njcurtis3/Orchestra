import { render, screen } from '@testing-library/react-native';
import { StyleSheet } from 'react-native';

import { Touchable } from '@/components/touchable';
import { MinTouchTarget } from '@/constants/theme';

describe('Touchable', () => {
  it('is at least 48dp in both dimensions, even when styled smaller', async () => {
    await render(<Touchable accessibilityLabel="Approve" style={{ width: 10, height: 10 }} />);
    const style = StyleSheet.flatten(screen.getByLabelText('Approve').props.style);
    expect(style.minHeight).toBeGreaterThanOrEqual(48);
    expect(style.minWidth).toBeGreaterThanOrEqual(48);
    expect(MinTouchTarget).toBeGreaterThanOrEqual(48);
  });

  it('carries its accessibility label and a default button role', async () => {
    await render(<Touchable accessibilityLabel="Approve" />);
    expect(screen.getByRole('button', { name: 'Approve' })).toBeTruthy();
  });
});
