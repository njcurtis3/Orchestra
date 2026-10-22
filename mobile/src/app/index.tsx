import { useRouter } from 'expo-router';
import { useEffect, useMemo, useRef, useState } from 'react';
import { FlatList, StyleSheet, View } from 'react-native';
import { SafeAreaView } from 'react-native-safe-area-context';

import { AlertsCard } from '@/components/alerts-card';
import { InboxRow } from '@/components/inbox-row';
import { LinkBanner, type BannerAction } from '@/components/link-banner';
import { SessionRow } from '@/components/session-row';
import { ThemedText } from '@/components/themed-text';
import { ThemedView } from '@/components/themed-view';
import { Touchable } from '@/components/touchable';
import { copy, extra, fill, splitActions } from '@/constants/copy';
import { MaxContentWidth, Spacing } from '@/constants/theme';
import { useTheme } from '@/hooks/use-theme';
import { listedItems } from '@/inbox/answer';
import { useLink } from '@/link/provider';

// Ticks once a second while active, for the retry countdown.
function useNow(active: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    setNow(Date.now());
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [active]);
  return now;
}

type Banner = { text: string; actions: BannerAction[] };

function banner(
  template: string,
  values: Record<string, string | number>,
  handlers: (() => void)[] = [],
): Banner {
  const { text, actions } = splitActions(template);
  return {
    text: fill(text, values),
    actions: actions.map((label, i) => ({ label, onPress: handlers[i] })),
  };
}

export default function HomeScreen() {
  const link = useLink();
  const router = useRouter();
  const theme = useTheme();
  const now = useNow(link.phase === 'waiting');

  // A revoke clears the host, but the banner still names it.
  const lastName = useRef<string | null>(null);
  if (link.host) lastName.current = link.host.host_name;
  const hostName = link.host?.host_name ?? lastName.current ?? extra.unknown_host;

  const sessions = useMemo(
    () => [...link.sessions].sort((a, b) => b.last_activity_at - a.last_activity_at),
    [link.sessions],
  );

  const needYou = useMemo(() => listedItems(link.inbox, link.outbox), [link.inbox, link.outbox]);

  if (link.phase === 'loading') return <ThemedView style={styles.container} />;

  const toPair = () => router.push('/pair');
  const values = { host: hostName };
  let notice: Banner | null = null;
  if (link.error?.kind === 'Revoked') {
    notice = banner(copy.revoked, values);
  } else if (link.host) {
    switch (link.phase) {
      case 'offline':
        notice = banner(copy.offline, values);
        break;
      case 'connecting':
      case 'pairing':
        notice = banner(copy.reconnecting, values);
        break;
      case 'waiting':
        if (link.error?.kind === 'VersionMismatch') {
          const supported = link.error.supported.join(', ');
          notice = banner(copy.version_mismatch, { ...values, supported });
        } else {
          const n = Math.max(0, Math.ceil(((link.retryAt ?? now) - now) / 1000));
          notice = banner(copy.unreachable, { ...values, n }, [link.retryNow]);
        }
        break;
      case 'not_paired':
        notice = banner(copy.not_paired_with_host, values, [toPair, () => void link.forget()]);
        break;
      case 'live':
        if (!link.scopes.includes('read')) notice = banner(copy.no_read_scope, values);
        break;
    }
  }

  if (!link.host) {
    return (
      <ThemedView style={styles.container}>
        <SafeAreaView style={styles.safeArea}>
          {notice ? <LinkBanner text={notice.text} actions={notice.actions} /> : null}
          <View style={styles.centered}>
            <ThemedText type="subtitle" accessibilityRole="header">
              {copy.not_paired_title}
            </ThemedText>
            <ThemedText>{copy.not_paired_body}</ThemedText>
            <ThemedText themeColor="textSecondary">{copy.not_paired_hint}</ThemedText>
            <Touchable
              accessibilityLabel={copy.not_paired_button}
              onPress={toPair}
              style={[styles.primary, { backgroundColor: theme.backgroundSelected }]}>
              <ThemedText type="smallBold">{copy.not_paired_button}</ThemedText>
            </Touchable>
          </View>
        </SafeAreaView>
      </ThemedView>
    );
  }

  const showEmpty = link.phase === 'live' && link.scopes.includes('read');
  return (
    <ThemedView style={styles.container}>
      <SafeAreaView style={styles.safeArea}>
        <View style={styles.header}>
          <ThemedText type="subtitle" accessibilityRole="header" numberOfLines={1}>
            {link.host.host_name}
          </ThemedText>
          <ThemedText themeColor="textSecondary">{extra.need_you(link.inboxCount)}</ThemedText>
        </View>
        {notice ? <LinkBanner text={notice.text} actions={notice.actions} /> : null}
        <FlatList
          data={sessions}
          ListHeaderComponent={
            needYou.length > 0 || showEmpty ? (
              <View style={styles.section}>
                <ThemedText type="smallBold" accessibilityRole="header">
                  {copy.inbox_title}
                </ThemedText>
                {needYou.length === 0 ? (
                  <ThemedText themeColor="textSecondary">{copy.inbox_empty}</ThemedText>
                ) : (
                  needYou.map((item) => (
                    <InboxRow
                      key={item.id}
                      item={item}
                      project={
                        link.sessions.find((x) => x.id === item.session_id)?.project_name ??
                        extra.unknown_project
                      }
                      entry={link.outbox[item.id]}
                      onPress={() =>
                        router.push({ pathname: '/inbox/[id]', params: { id: item.id } })
                      }
                    />
                  ))
                )}
              </View>
            ) : null
          }
          ListFooterComponent={<AlertsCard />}
          keyExtractor={(s) => s.id}
          renderItem={({ item }) => <SessionRow session={item} />}
          ItemSeparatorComponent={() => <View style={styles.gap} />}
          ListEmptyComponent={
            showEmpty ? (
              <ThemedText themeColor="textSecondary">
                {fill(copy.no_sessions, { host: link.host.host_name })}
              </ThemedText>
            ) : null
          }
        />
      </SafeAreaView>
    </ThemedView>
  );
}

const styles = StyleSheet.create({
  container: {
    flex: 1,
  },
  safeArea: {
    flex: 1,
    alignSelf: 'center',
    width: '100%',
    maxWidth: MaxContentWidth,
    paddingHorizontal: Spacing.four,
    paddingTop: Spacing.three,
    gap: Spacing.three,
  },
  centered: {
    flex: 1,
    justifyContent: 'center',
    gap: Spacing.three,
  },
  header: {
    gap: Spacing.one,
  },
  gap: {
    height: Spacing.two,
  },
  section: {
    gap: Spacing.two,
    paddingBottom: Spacing.three,
  },
  primary: {
    borderRadius: Spacing.three,
    paddingHorizontal: Spacing.four,
    alignSelf: 'flex-start',
  },
});
