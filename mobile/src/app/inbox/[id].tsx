import { useLocalSearchParams, useRouter } from 'expo-router';
import { useEffect } from 'react';
import { KeyboardAvoidingView, Platform, ScrollView, StyleSheet, View } from 'react-native';
import { SafeAreaView } from 'react-native-safe-area-context';

import { AnswerPermission } from '@/components/answer-permission';
import { AnswerQuestion } from '@/components/answer-question';
import { AnswerStatus } from '@/components/answer-status';
import { kindLabel } from '@/components/inbox-row';
import { ThemedText } from '@/components/themed-text';
import { ThemedView } from '@/components/themed-view';
import { Touchable } from '@/components/touchable';
import { copy, extra, fill, splitActions } from '@/constants/copy';
import { MaxContentWidth, Spacing } from '@/constants/theme';
import { useTheme } from '@/hooks/use-theme';
import {
  allowAnswer,
  denyAnswer,
  draftFromAnswer,
  emptyQuestionDraft,
  questionAnswer,
} from '@/inbox/answer';
import { useDraft } from '@/inbox/drafts';
import { useLink } from '@/link/provider';

const plain = (template: string, host: string) => fill(splitActions(template).text, { host });

export default function InboxItemScreen() {
  const { id } = useLocalSearchParams<{ id: string }>();
  const link = useLink();
  const router = useRouter();
  const theme = useTheme();
  const [draft, change] = useDraft(id);

  const item = link.inbox.find((i) => i.id === id);
  const entry = link.outbox[id];
  const host = link.host?.host_name ?? extra.unknown_host;
  const session = item ? link.sessions.find((s) => s.id === item.session_id) : undefined;
  const project = session?.project_name ?? extra.unknown_project;

  // A held answer fills the controls when the draft is empty (M6): a queued or failed answer shows
  // what is waiting to go.
  const blank =
    draft.message === '' &&
    Object.keys(draft.question.picks).length === 0 &&
    Object.keys(draft.question.other).length === 0;
  useEffect(() => {
    if (!entry || !blank) return;
    const a = entry.answer;
    if (a.kind === 'permission') {
      change((d) => ({ ...d, message: a.decision === 'deny' ? (a.message ?? '') : '' }));
    } else if (item?.questions) {
      const questions = item.questions;
      change((d) => ({ ...d, question: draftFromAnswer(questions, a.answers) }));
    }
  }, [entry, blank, item, change]);

  const back = (
    <Touchable
      accessibilityLabel={copy.back}
      onPress={() => router.back()}
      style={[styles.back, { backgroundColor: theme.backgroundElement }]}>
      <ThemedText type="smallBold">{copy.back}</ThemedText>
    </Touchable>
  );

  const status = entry ? (
    <AnswerStatus
      entry={entry}
      host={host}
      onCancel={() => link.cancelAnswer(id)}
      onRetry={() => link.retryAnswer(id)}
      onDismiss={() => link.dismissAnswer(id)}
    />
  ) : null;

  if (!item) {
    return (
      <Screen>
        {back}
        {status ?? <ThemedText>{plain(copy.gone, host)}</ThemedText>}
      </Screen>
    );
  }

  const pending = !item.resolved && !item.expired;
  const disabled = !!entry;
  const questions = item.questions ?? [];
  let controls = null;
  if (pending && !link.canAnswer) {
    controls = <ThemedText>{fill(copy.no_answer_scope, { host })}</ThemedText>;
  } else if (pending && item.kind === 'permission') {
    controls = (
      <AnswerPermission
        tool={item.tool_name}
        message={draft.message}
        disabled={disabled}
        onMessage={(t) => change((d) => ({ ...d, message: t }))}
        onAllow={() => link.answer(allowAnswer(item.id))}
        onDeny={() => link.answer(denyAnswer(item.id, draft.message))}
      />
    );
  } else if (pending && item.kind === 'question') {
    controls = (
      <AnswerQuestion
        questions={questions}
        draft={draft.question ?? emptyQuestionDraft()}
        disabled={disabled}
        onChange={(update) => change((d) => ({ ...d, question: update(d.question) }))}
        onSend={(answers) => link.answer(questionAnswer(item.id, answers))}
      />
    );
  }

  // A resolved or expired item has nothing to answer; say why, unless the status line already has.
  let closed = null;
  if (!pending && !entry) {
    closed = <ThemedText>{plain(item.expired ? copy.expired : copy.answered, host)}</ThemedText>;
  }

  return (
    <Screen>
      {back}
      <View style={styles.head}>
        <ThemedText type="subtitle" accessibilityRole="header">
          {kindLabel(item)}
        </ThemedText>
        <ThemedText type="smallBold">{project}</ThemedText>
        {item.summary ? <ThemedText>{item.summary}</ThemedText> : null}
        {item.tool_name ? (
          <ThemedText type="small" themeColor="textSecondary">
            {`${extra.tool_label}: ${item.tool_name}`}
          </ThemedText>
        ) : null}
        {item.tool_input_preview ? (
          <ScrollView
            nestedScrollEnabled
            accessibilityLabel={extra.input_label}
            style={[styles.preview, { backgroundColor: theme.backgroundElement }]}>
            <ThemedText type="code" selectable>
              {item.tool_input_preview}
            </ThemedText>
          </ScrollView>
        ) : null}
      </View>
      {status}
      {closed}
      {controls}
    </Screen>
  );
}

function Screen({ children }: { children: React.ReactNode }) {
  return (
    <ThemedView style={styles.container}>
      <SafeAreaView style={styles.safeArea}>
        <KeyboardAvoidingView
          behavior={Platform.OS === 'ios' ? 'padding' : 'height'}
          style={styles.container}>
          <ScrollView keyboardShouldPersistTaps="handled" contentContainerStyle={styles.content}>
            {children}
          </ScrollView>
        </KeyboardAvoidingView>
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
  },
  content: {
    gap: Spacing.three,
    paddingBottom: Spacing.five,
  },
  back: {
    alignSelf: 'flex-start',
    borderRadius: Spacing.three,
    paddingHorizontal: Spacing.three,
  },
  head: {
    gap: Spacing.two,
  },
  preview: {
    maxHeight: 200,
    borderRadius: Spacing.two,
    padding: Spacing.two,
  },
});
