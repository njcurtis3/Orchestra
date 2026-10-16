import { Pressable, type PressableProps } from 'react-native';

import { MinTouchSpacing, MinTouchTarget } from '@/constants/theme';

export type TouchableProps = Omit<PressableProps, 'accessibilityLabel' | 'accessibilityRole'> & {
  /** Required: every control is announced by screen readers. */
  accessibilityLabel: string;
  accessibilityRole?: PressableProps['accessibilityRole'];
};

/** Pressable that never renders smaller than the 48dp Android minimum touch target. */
export function Touchable({ style, accessibilityRole = 'button', ...rest }: TouchableProps) {
  return (
    <Pressable
      accessibilityRole={accessibilityRole}
      hitSlop={MinTouchSpacing}
      style={(state) => [
        { minHeight: MinTouchTarget, minWidth: MinTouchTarget, justifyContent: 'center', alignItems: 'center' },
        typeof style === 'function' ? style(state) : style,
      ]}
      {...rest}
    />
  );
}
