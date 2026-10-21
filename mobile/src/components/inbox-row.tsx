import { StyleSheet, View } from 'react-native';

import type { InboxItem } from '../../../protocol/types';
import { ThemedText } from '@/components/themed-text';
import { Touchable } from '@/components/touchable';
import { copy, extra, fill } from '@/constants/copy';
import { Spacing } from '@/constants/theme';
import { useTheme } from '@/hooks/use-theme';
import type { OutboxEntry } from '@/link/outbox';

export function kindLabel(item: InboxItem): string {
  return item.kind === 'question' ? copy.kind_question : copy.kind_permission;
}

export function rowStatus(entry: OutboxEntry | undefined): string {
  const table: Record<string, string> = extra.row_status;
  return entry ? (table[entry.status] ?? entry.status) : extra.row_status.pending;
}

type Props = {
  item: InboxItem;
  project: string;
  entry?: OutboxEntry;
  onPress: () => void;
};

export function InboxRow({ item, project, entry, onPress }: Props) {
  const theme = useTheme();
  const status = rowStatus(entry);
  const title = fill(copy.row_label, { kind: kindLabel(item), project });
  return (
    <Touchable
      accessibilityLabel={`${title}, ${status}`}
      onPress={onPress}
      style={[styles.row, { backgroundColor: theme.backgroundElement }]}>
      <View style={styles.inner}>
        <View style={styles.top}>
          <ThemedText type="smallBold" numberOfLines={1} style={styles.name}>
            {title}
          </ThemedText>
          <ThemedText type="small" themeColor="textSecondary">
            {status}
          </ThemedText>
        </View>
        {item.summary ? (
          <ThemedText type="small" themeColor="textSecondary" numberOfLines={2}>
            {item.summary}
          </ThemedText>
        ) : null}
      </View>
    </Touchable>
  );
}

const styles = StyleSheet.create({
  row: {
    alignItems: 'stretch',
    borderRadius: Spacing.three,
    padding: Spacing.three,
  },
  inner: {
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
