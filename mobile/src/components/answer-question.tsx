import { StyleSheet, TextInput, View } from 'react-native';

import type { Question } from '../../../protocol/types';
import { ThemedText } from '@/components/themed-text';
import { Touchable } from '@/components/touchable';
import { copy } from '@/constants/copy';
import { Spacing } from '@/constants/theme';
import { useTheme } from '@/hooks/use-theme';
import {
  collect,
  isComplete,
  questionLimit,
  setOther,
  togglePick,
  type QuestionDraft,
} from '@/inbox/answer';

type Props = {
  questions: Question[];
  draft: QuestionDraft;
  disabled: boolean;
  onChange: (update: (d: QuestionDraft) => QuestionDraft) => void;
  onSend: (answers: Record<string, string>) => void;
};

export function AnswerQuestion({ questions, draft, disabled, onChange, onSend }: Props) {
  const theme = useTheme();
  if (questions.length === 0) {
    return <ThemedText themeColor="textSecondary">{copy.no_questions}</ThemedText>;
  }

  const answers = collect(questions, draft);
  const limit = questionLimit(questions, answers);
  const canSend = !disabled && isComplete(questions, answers) && limit === 'ok';

  return (
    <View style={styles.box}>
      {questions.map((q, qi) => {
        const picked = draft.picks[q.question] ?? [];
        return (
          <View key={qi} style={styles.group}>
            {q.header ? (
              <ThemedText type="small" themeColor="textSecondary" accessibilityRole="header">
                {q.header}
              </ThemedText>
            ) : null}
            <ThemedText type="smallBold" accessibilityRole="header">
              {q.question}
            </ThemedText>
            {q.options.map((o) => {
              const checked = picked.includes(o.label);
              const label = o.description ? `${o.label}. ${o.description}` : o.label;
              return (
                <Touchable
                  key={o.label}
                  accessibilityRole={q.multi_select ? 'checkbox' : 'radio'}
                  accessibilityLabel={label}
                  accessibilityState={{ checked, disabled }}
                  disabled={disabled}
                  onPress={() => onChange((d) => togglePick(d, q, o.label, !checked))}
                  style={[
                    styles.option,
                    { backgroundColor: checked ? theme.backgroundSelected : theme.backgroundElement },
                  ]}>
                  <ThemedText type="smallBold">{`${checked ? '[x]' : '[ ]'} ${o.label}`}</ThemedText>
                  {o.description ? (
                    <ThemedText type="small" themeColor="textSecondary">
                      {o.description}
                    </ThemedText>
                  ) : null}
                </Touchable>
              );
            })}
            <TextInput
              accessibilityLabel={`${copy.other}: ${q.question}`}
              placeholder={copy.other}
              placeholderTextColor={theme.textSecondary}
              value={draft.other[q.question] ?? ''}
              onChangeText={(t) => onChange((d) => setOther(d, q, t))}
              editable={!disabled}
              multiline
              style={[styles.input, { color: theme.text, backgroundColor: theme.backgroundElement }]}
            />
          </View>
        );
      })}
      {limit === 'too_long' ? <ThemedText type="small">{copy.answer_too_long}</ThemedText> : null}
      {limit === 'too_many' ? <ThemedText type="small">{copy.too_many_questions}</ThemedText> : null}
      <Touchable
        accessibilityLabel={copy.send_answers}
        disabled={!canSend}
        onPress={() => onSend(answers)}
        style={[styles.send, { backgroundColor: theme.backgroundSelected }]}>
        <ThemedText type="smallBold">{copy.send_answers}</ThemedText>
      </Touchable>
    </View>
  );
}

const styles = StyleSheet.create({
  box: {
    gap: Spacing.three,
  },
  group: {
    gap: Spacing.two,
  },
  option: {
    alignItems: 'flex-start',
    borderRadius: Spacing.two,
    paddingHorizontal: Spacing.three,
    paddingVertical: Spacing.two,
  },
  input: {
    borderRadius: Spacing.two,
    padding: Spacing.three,
    minHeight: 48,
    fontSize: 16,
    textAlignVertical: 'top',
  },
  send: {
    borderRadius: Spacing.three,
    paddingHorizontal: Spacing.four,
    alignSelf: 'flex-start',
  },
});
