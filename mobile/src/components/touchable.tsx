import { Pressable, StyleSheet, type PressableProps } from 'react-native';

import { MinTouchSpacing, MinTouchTarget } from '@/constants/theme';

export type TouchableProps = Omit<PressableProps, 'accessibilityLabel' | 'accessibilityRole'> & {
  /** Required: every control is announced by screen readers. */
  accessibilityLabel: string;
  accessibilityRole?: PressableProps['accessibilityRole'];
};

const slopEdges = ['top', 'bottom', 'left', 'right'] as const;

function withMinSlop(hitSlop: PressableProps['hitSlop']) {
  const given = typeof hitSlop === 'number' ? { top: hitSlop, bottom: hitSlop, left: hitSlop, right: hitSlop } : hitSlop;
  return Object.fromEntries(slopEdges.map((edge) => [edge, Math.max(given?.[edge] ?? 0, MinTouchSpacing)]));
}

/** Pressable that never renders smaller than the 48dp Android minimum touch target, whatever the caller passes. */
export function Touchable({ style, hitSlop, accessibilityRole = 'button', ...rest }: TouchableProps) {
  return (
    <Pressable
      accessibilityRole={accessibilityRole}
      {...rest}
      hitSlop={withMinSlop(hitSlop)}
      style={(state) => {
        const flat = StyleSheet.flatten(typeof style === 'function' ? style(state) : style);
        return [
          flat,
          {
            justifyContent: 'center',
            alignItems: 'center',
            // applied last and clamped, so a caller's smaller size cannot win
            minHeight: Math.max(Number(flat?.minHeight) || 0, MinTouchTarget),
            minWidth: Math.max(Number(flat?.minWidth) || 0, MinTouchTarget),
            ...(typeof flat?.height === 'number' && { height: Math.max(flat.height, MinTouchTarget) }),
            ...(typeof flat?.width === 'number' && { width: Math.max(flat.width, MinTouchTarget) }),
          },
        ];
      }}
    />
  );
}
