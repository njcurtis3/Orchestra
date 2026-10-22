import { useCallback, useEffect, useState } from 'react';
import { AppState, Linking, StyleSheet, View } from 'react-native';

import { ThemedText } from '@/components/themed-text';
import { Touchable } from '@/components/touchable';
import { copy, splitActions } from '@/constants/copy';
import { Spacing } from '@/constants/theme';
import { useTheme } from '@/hooks/use-theme';
import { getStatus, request, type AlertStatus } from '@/notify/os';

// "Not now" lasts this app run only. In memory, never written to disk.
let hiddenThisRun = false;

export function resetAlertsHidden(): void {
  hiddenThisRun = false;
}

function Action({ label, onPress }: { label: string; onPress: () => void }) {
  const theme = useTheme();
  return (
    <Touchable
      accessibilityLabel={label}
      onPress={onPress}
      style={[styles.action, { backgroundColor: theme.backgroundElement }]}>
      <ThemedText type="smallBold">{label}</ThemedText>
    </Touchable>
  );
}

export function AlertsCard() {
  const theme = useTheme();
  const [status, setStatus] = useState<AlertStatus | null>(null);
  const [hidden, setHidden] = useState(hiddenThisRun);

  const refresh = useCallback(() => {
    getStatus().then(setStatus, () => undefined);
  }, []);

  useEffect(() => {
    refresh();
    // Re-read after a return from Settings.
    const sub = AppState.addEventListener('change', (s) => {
      if (s === 'active') refresh();
    });
    return () => sub.remove();
  }, [refresh]);

  if (!status || hidden) return null;

  const turnOn = () => {
    request().then(setStatus, () => undefined);
  };
  const notNow = () => {
    hiddenThisRun = true;
    setHidden(true);
  };
  const openSettings = () => {
    void Linking.openSettings();
  };

  let body: React.ReactNode;
  if (status.status === 'granted') {
    const { text, actions } = splitActions(copy.alerts_on);
    body = (
      <>
        <ThemedText type="small">{text}</ThemedText>
        <View style={styles.actions}>
          {actions.map((a) => (
            <Action key={a} label={a} onPress={openSettings} />
          ))}
        </View>
      </>
    );
  } else if (status.status === 'denied') {
    const template = status.canAskAgain ? copy.alerts_denied_retry : copy.alerts_denied_settings;
    const { text, actions } = splitActions(template);
    body = (
      <>
        <ThemedText type="small">{text}</ThemedText>
        <View style={styles.actions}>
          {actions.map((a) => (
            <Action key={a} label={a} onPress={status.canAskAgain ? turnOn : openSettings} />
          ))}
        </View>
      </>
    );
  } else {
    body = (
      <>
        <ThemedText type="smallBold" accessibilityRole="header">
          {copy.alerts_title}
        </ThemedText>
        <ThemedText type="small">{copy.alerts_body}</ThemedText>
        <View style={styles.actions}>
          <Action label={copy.alerts_button} onPress={turnOn} />
          <Action label={copy.alerts_not_now} onPress={notNow} />
        </View>
      </>
    );
  }

  return <View style={[styles.card, { backgroundColor: theme.backgroundSelected }]}>{body}</View>;
}

const styles = StyleSheet.create({
  card: {
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
