import { render, screen } from '@testing-library/react-native';
import { StyleSheet } from 'react-native';

import { Touchable } from '@/components/touchable';
import { MinTouchSpacing, MinTouchTarget } from '@/constants/theme';

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

  it('overrides an explicit sub-48 size and hitSlop={0}', async () => {
    await render(
      <Touchable
        accessibilityLabel="Tiny"
        hitSlop={0}
        style={{ width: 20, height: 20, minWidth: 20, minHeight: 20 }}
      />,
    );
    const node = screen.getByLabelText('Tiny');
    const style = StyleSheet.flatten(node.props.style);
    expect(style.width).toBeGreaterThanOrEqual(48);
    expect(style.height).toBeGreaterThanOrEqual(48);
    expect(style.minWidth).toBeGreaterThanOrEqual(48);
    expect(style.minHeight).toBeGreaterThanOrEqual(48);
    expect(MinTouchSpacing).toBeGreaterThanOrEqual(8);
  });
});
