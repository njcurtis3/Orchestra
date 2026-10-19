import { StyleSheet, View } from 'react-native';

import type { SessionSummary } from '../../../protocol/types';
import { ThemedText } from '@/components/themed-text';
import { statusLabel } from '@/constants/copy';
import { Spacing } from '@/constants/theme';
import { useTheme } from '@/hooks/use-theme';

export function SessionRow({ session }: { session: SessionSummary }) {
  const theme = useTheme();
  const label = statusLabel(session.status);
  return (
    <View
      accessible
      accessibilityLabel={`${session.project_name}, ${label}`}
      style={[styles.row, { backgroundColor: theme.backgroundElement }]}>
      <View style={styles.top}>
        <ThemedText type="smallBold" numberOfLines={1} style={styles.name}>
          {session.project_name}
        </ThemedText>
        <ThemedText type="small" themeColor="textSecondary">
          {label}
        </ThemedText>
      </View>
      {session.last_message ? (
        <ThemedText type="small" themeColor="textSecondary" numberOfLines={1}>
          {session.last_message}
        </ThemedText>
      ) : null}
    </View>
  );
}

const styles = StyleSheet.create({
  row: {
    borderRadius: Spacing.three,
    padding: Spacing.three,
    gap: Spacing.one,
  },
  top: {
    flexDirection: 'row',
    justifyContent: 'space-between',
    gap: Spacing.two,
  },
  name: {
    flexShrink: 1,
  },
});
