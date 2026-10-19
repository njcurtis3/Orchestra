import { StyleSheet, View } from 'react-native';

import { ThemedText } from '@/components/themed-text';
import { Touchable } from '@/components/touchable';
import { Spacing } from '@/constants/theme';
import { useTheme } from '@/hooks/use-theme';

export type BannerAction = { label: string; onPress: () => void };

export function LinkBanner({ text, actions = [] }: { text: string; actions?: BannerAction[] }) {
  const theme = useTheme();
  return (
    <View
      accessibilityRole="alert"
      accessibilityLiveRegion="polite"
      style={[styles.banner, { backgroundColor: theme.backgroundSelected }]}>
      <ThemedText type="small">{text}</ThemedText>
      <View style={styles.actions}>
        {actions.map((a) => (
          <Touchable
            key={a.label}
            accessibilityLabel={a.label}
            onPress={a.onPress}
            style={[styles.action, { backgroundColor: theme.backgroundElement }]}>
            <ThemedText type="smallBold">{a.label}</ThemedText>
          </Touchable>
        ))}
      </View>
    </View>
  );
}

const styles = StyleSheet.create({
  banner: {
    borderRadius: Spacing.three,
    padding: Spacing.three,
    gap: Spacing.two,
  },
  actions: {
    flexDirection: 'row',
    flexWrap: 'wrap',
    gap: Spacing.two,
  },
  action: {
    borderRadius: Spacing.two,
    paddingHorizontal: Spacing.three,
  },
});
