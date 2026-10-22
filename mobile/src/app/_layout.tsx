import { DarkTheme, DefaultTheme, Stack, ThemeProvider, usePathname, useRouter } from 'expo-router';
import { useEffect } from 'react';
import { useColorScheme } from 'react-native';
import { SafeAreaProvider } from 'react-native-safe-area-context';

import { LinkProvider } from '@/link/provider';
import { notifier } from '@/notify/notifier';
import { onTap, takeLaunchTap } from '@/notify/os';
import { parseTap } from '@/notify/payload';

// Taps and route changes. A bad payload goes home; a gone item shows the item screen's gone state.
function NotifyBridge() {
  const router = useRouter();
  const pathname = usePathname();

  useEffect(() => {
    notifier.setRoute(pathname);
  }, [pathname]);

  useEffect(() => {
    const open = (data: unknown) => {
      const id = parseTap(data);
      if (id) router.push({ pathname: '/inbox/[id]', params: { id } });
      else router.replace('/');
    };
    let live = true;
    takeLaunchTap().then(
      (tap) => {
        if (live && tap) open(tap.data);
      },
      () => undefined,
    );
    const off = onTap(open);
    return () => {
      live = false;
      off();
    };
  }, [router]);

  return null;
}

export default function RootLayout() {
  const colorScheme = useColorScheme();
  return (
    <SafeAreaProvider>
      <ThemeProvider value={colorScheme === 'dark' ? DarkTheme : DefaultTheme}>
        <LinkProvider>
          <NotifyBridge />
          <Stack screenOptions={{ headerShown: false }} />
        </LinkProvider>
      </ThemeProvider>
    </SafeAreaProvider>
  );
}
