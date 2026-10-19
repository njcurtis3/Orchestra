import { CameraView, useCameraPermissions } from 'expo-camera';
import { useRouter } from 'expo-router';
import { useEffect, useRef, useState, type ReactNode } from 'react';
import { Linking, StyleSheet, View } from 'react-native';
import { SafeAreaView } from 'react-native-safe-area-context';

import { ThemedText } from '@/components/themed-text';
import { ThemedView } from '@/components/themed-view';
import { Touchable } from '@/components/touchable';
import { copy, countdown, fill, splitActions } from '@/constants/copy';
import { MaxContentWidth, Spacing } from '@/constants/theme';
import { useTheme } from '@/hooks/use-theme';
import type { LinkFailure } from '@/link/client';
import { parseOffer, type PairingOffer } from '@/link/offer';
import { useLink } from '@/link/provider';

type Phase =
  | { kind: 'scanning' }
  | { kind: 'rejected'; text: string }
  | { kind: 'working'; offer: PairingOffer }
  | { kind: 'paired'; host: string }
  | { kind: 'failed'; text: string };

const SCAN_AGAIN = splitActions(copy.pair_not_confirmed).actions[0];

function failureText(failure: LinkFailure | { kind: 'StoreFailed' }, offer: PairingOffer): string {
  switch (failure.kind) {
    case 'PairWindowClosed':
      return copy.pair_window_closed;
    case 'PairAuthFailed':
      return copy.pair_auth_failed;
    case 'Unreachable':
      return fill(copy.pair_unreachable, { host: offer.host_name, addr: offer.addrs[0] ?? '' });
    case 'VersionMismatch':
      return fill(copy.version_mismatch, {
        host: offer.host_name,
        supported: failure.supported.join(', '),
      });
    default:
      return splitActions(copy.pair_not_confirmed).text;
  }
}

export default function PairScreen() {
  const router = useRouter();
  const theme = useTheme();
  const link = useLink();
  const [permission, requestPermission] = useCameraPermissions();
  const [asked, setAsked] = useState(false);
  const [phase, setPhase] = useState<Phase>({ kind: 'scanning' });
  const [now, setNow] = useState(() => Date.now());
  const scanned = useRef(false);
  const mounted = useRef(true);

  // Leaving the screen, by Android back or any other way, cancels a pairing in progress.
  const cancelRef = useRef(link.cancelPair);
  cancelRef.current = link.cancelPair;
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      cancelRef.current();
    };
  }, []);

  const working = phase.kind === 'working';
  useEffect(() => {
    if (!working) return;
    setNow(Date.now());
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [working]);

  const button = (label: string, onPress: () => void): ReactNode => (
    <Touchable
      accessibilityLabel={label}
      onPress={onPress}
      style={[styles.button, { backgroundColor: theme.backgroundSelected }]}>
      <ThemedText type="smallBold">{label}</ThemedText>
    </Touchable>
  );

  const scanAgain = () => {
    scanned.current = false;
    setPhase({ kind: 'scanning' });
  };

  const onScanned = async (data: string) => {
    if (scanned.current) return;
    scanned.current = true;
    const parsed = parseOffer(data, Date.now());
    if (parsed.kind !== 'ok') {
      setPhase({ kind: 'rejected', text: copy[parsed.kind] });
      return;
    }
    const offer = parsed.offer;
    setPhase({ kind: 'working', offer });
    const outcome = await link.pair(offer);
    if (!mounted.current) return;
    if (outcome.ok) {
      setPhase({ kind: 'paired', host: outcome.host.host_name });
      router.replace('/');
    } else if (outcome.failure.kind !== 'Closed') {
      setPhase({ kind: 'failed', text: failureText(outcome.failure, offer) });
    }
  };

  const cancel = () => {
    link.cancelPair();
    router.back();
  };

  let body: ReactNode;
  if (!asked) {
    body = (
      <>
        <ThemedText type="subtitle" accessibilityRole="header">
          {copy.rationale_title}
        </ThemedText>
        <ThemedText>{copy.rationale_body}</ThemedText>
        {button(copy.rationale_button, () => {
          setAsked(true);
          void requestPermission();
        })}
      </>
    );
  } else if (phase.kind === 'working') {
    if (link.pairStep === 'confirm') {
      const wait = splitActions(copy.confirm_wait);
      body = (
        <>
          <ThemedText type="subtitle" accessibilityRole="header">
            {copy.confirm_title}
          </ThemedText>
          <ThemedText>
            {fill(copy.confirm_body, { fingerprint: link.pairFingerprint ?? '' })}
          </ThemedText>
          <ThemedText themeColor="textSecondary">
            {fill(wait.text, { 'm:ss': countdown(phase.offer.expires_at - now) })}
          </ThemedText>
          {button(wait.actions[0], cancel)}
        </>
      );
    } else {
      body = (
        <ThemedText accessibilityRole="header">
          {fill(copy.connecting, { host: phase.offer.host_name })}
        </ThemedText>
      );
    }
  } else if (phase.kind === 'paired') {
    body = <ThemedText accessibilityRole="header">{fill(copy.paired, { host: phase.host })}</ThemedText>;
  } else if (phase.kind === 'rejected' || phase.kind === 'failed') {
    body = (
      <>
        <ThemedText accessibilityRole="alert">{phase.text}</ThemedText>
        {button(SCAN_AGAIN, scanAgain)}
      </>
    );
  } else if (permission?.granted) {
    body = (
      <>
        <ThemedText accessibilityRole="header">{copy.scanning}</ThemedText>
        <CameraView
          style={styles.camera}
          facing="back"
          barcodeScannerSettings={{ barcodeTypes: ['qr'] }}
          onBarcodeScanned={({ data }) => void onScanned(data)}
        />
      </>
    );
  } else if (permission && permission.status === 'denied') {
    const denied = splitActions(permission.canAskAgain ? copy.denied_retry : copy.denied_settings);
    body = (
      <>
        <ThemedText>{denied.text}</ThemedText>
        {button(
          denied.actions[0],
          permission.canAskAgain ? () => void requestPermission() : () => void Linking.openSettings(),
        )}
      </>
    );
  } else {
    body = null;
  }

  return (
    <ThemedView style={styles.container}>
      <SafeAreaView style={styles.safeArea}>{body}</SafeAreaView>
    </ThemedView>
  );
}

const styles = StyleSheet.create({
  container: {
    flex: 1,
  },
  safeArea: {
    flex: 1,
    justifyContent: 'center',
    alignSelf: 'center',
    width: '100%',
    maxWidth: MaxContentWidth,
    paddingHorizontal: Spacing.four,
    gap: Spacing.three,
  },
  button: {
    borderRadius: Spacing.three,
    paddingHorizontal: Spacing.four,
    alignSelf: 'flex-start',
  },
  camera: {
    flex: 1,
    maxHeight: 400,
    borderRadius: Spacing.three,
    overflow: 'hidden',
  },
});
