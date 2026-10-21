import { StyleSheet, View } from 'react-native';

import { ThemedText } from '@/components/themed-text';
import { Touchable } from '@/components/touchable';
import { copy, extra, fill, splitActions } from '@/constants/copy';
import { Spacing } from '@/constants/theme';
import { useTheme } from '@/hooks/use-theme';
import type { OutboxEntry } from '@/link/outbox';

type Props = {
  entry: OutboxEntry;
  host: string;
  onCancel: () => void;
  onRetry: () => void;
  onDismiss: () => void;
};

function template(entry: OutboxEntry): string {
  switch (entry.status) {
    case 'queued':
      return copy.queued;
    case 'sending':
      return copy.sending;
    case 'unconfirmed':
      return copy.unconfirmed;
    case 'sent':
      return copy.sent;
    case 'failed':
      return copy.failed;
    case 'refused':
      return `${copy.no_answer_scope} [Dismiss]`;
    case 'answered':
      return copy.answered;
    case 'expired':
      return copy.expired;
    case 'gone':
      return copy.gone;
  }
}

// The outbox status in words, announced politely to screen readers, with the actions it allows.
export function AnswerStatus({ entry, host, onCancel, onRetry, onDismiss }: Props) {
  const theme = useTheme();
  const { text, actions } = splitActions(template(entry));
  const handlers: Record<string, () => void> = {
    Cancel: onCancel,
    'Try again': onRetry,
    Dismiss: onDismiss,
  };
  // The host's reason is plain text from the host; the answer itself is never in it.
  const reason = entry.message?.trim() || extra.missing_reason;
  // accessibilityLiveRegion is Android-only; iOS would need AccessibilityInfo.announceForAccessibility.
  return (
    <View
      accessibilityLiveRegion="polite"
      style={[styles.box, { backgroundColor: theme.backgroundSelected }]}>
      <ThemedText type="small">{fill(text, { host, reason })}</ThemedText>
      <View style={styles.actions}>
        {actions.map((label) => (
          <Touchable
            key={label}
            accessibilityLabel={label}
            onPress={handlers[label]}
            style={[styles.action, { backgroundColor: theme.backgroundElement }]}>
            <ThemedText type="smallBold">{label}</ThemedText>
          </Touchable>
        ))}
      </View>
    </View>
  );
}

const styles = StyleSheet.create({
  box: {
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
