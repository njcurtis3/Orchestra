import { DarkTheme, DefaultTheme, Stack, ThemeProvider } from 'expo-router';
import { useColorScheme } from 'react-native';
import { SafeAreaProvider } from 'react-native-safe-area-context';

import { LinkProvider } from '@/link/provider';

export default function RootLayout() {
  const colorScheme = useColorScheme();
  return (
    <SafeAreaProvider>
      <ThemeProvider value={colorScheme === 'dark' ? DarkTheme : DefaultTheme}>
        <LinkProvider>
          <Stack screenOptions={{ headerShown: false }} />
        </LinkProvider>
      </ThemeProvider>
    </SafeAreaProvider>
  );
}
