import { StyleSheet, TextInput, View } from 'react-native';

import { ThemedText } from '@/components/themed-text';
import { Touchable } from '@/components/touchable';
import { copy } from '@/constants/copy';
import { Spacing } from '@/constants/theme';
import { useTheme } from '@/hooks/use-theme';
import { denyTooLong } from '@/inbox/answer';

type Props = {
  tool: string | null;
  message: string;
  disabled: boolean;
  onMessage: (text: string) => void;
  onAllow: () => void;
  onDeny: () => void;
};

export function AnswerPermission({ tool, message, disabled, onMessage, onAllow, onDeny }: Props) {
  const theme = useTheme();
  const long = denyTooLong(message);
  const allowLabel = tool ? `${copy.allow} ${tool}` : copy.allow;
  const denyLabel = tool ? `${copy.deny} ${tool}` : copy.deny;
  return (
    <View style={styles.box}>
      <TextInput
        accessibilityLabel={copy.deny_message}
        placeholder={copy.deny_message}
        placeholderTextColor={theme.textSecondary}
        value={message}
        onChangeText={onMessage}
        editable={!disabled}
        multiline
        style={[styles.input, { color: theme.text, backgroundColor: theme.backgroundElement }]}
      />
      {long ? <ThemedText type="small">{copy.deny_too_long}</ThemedText> : null}
      <View style={styles.buttons}>
        <Touchable
          accessibilityLabel={allowLabel}
          disabled={disabled}
          onPress={onAllow}
          style={[styles.button, { backgroundColor: theme.backgroundSelected }]}>
          <ThemedText type="smallBold">{copy.allow}</ThemedText>
        </Touchable>
        <Touchable
          accessibilityLabel={denyLabel}
          disabled={disabled || long}
          onPress={onDeny}
          style={[styles.button, { backgroundColor: theme.backgroundSelected }]}>
          <ThemedText type="smallBold">{copy.deny}</ThemedText>
        </Touchable>
      </View>
    </View>
  );
}

const styles = StyleSheet.create({
  box: {
    gap: Spacing.two,
  },
  input: {
    borderRadius: Spacing.two,
    padding: Spacing.three,
    minHeight: 48,
    fontSize: 16,
    textAlignVertical: 'top',
  },
  buttons: {
    flexDirection: 'row',
    gap: Spacing.two,
  },
  button: {
    flex: 1,
    borderRadius: Spacing.three,
    paddingHorizontal: Spacing.three,
  },
});
