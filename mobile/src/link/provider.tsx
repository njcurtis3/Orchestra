// Wires the controller to the app: expo-secure-store, expo-crypto, expo-network, AppState,
// expo-device and expo-constants. The screens read it through useLink().
import Constants from 'expo-constants';
import * as Device from 'expo-device';
import * as Network from 'expo-network';
import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  useState,
  useSyncExternalStore,
  type ReactNode,
} from 'react';
import { AppState } from 'react-native';

import type { LinkFailure } from './client';
import { LinkController } from './controller';
import type { ControllerPhase, HostSummary, PairOutcome } from './controller';
import type { PairingOffer } from './offer';
import { random } from './random';
import type { LinkData } from './state';
import { forgetHost, listHosts, saveHost } from './store';

export type LinkContextValue = {
  phase: ControllerPhase;
  host: HostSummary | null;
  sessions: LinkData['sessions'];
  // Pending inbox items: not resolved and not expired.
  inboxCount: number;
  error: LinkFailure | null;
  scopes: string[];
  pairStep: 'connecting' | 'confirm' | null;
  pairFingerprint: string | null;
  retryAt: number | null;
  pair: (offer: PairingOffer) => Promise<PairOutcome>;
  cancelPair: () => void;
  retryNow: () => void;
  forget: () => Promise<void>;
};

const LinkContext = createContext<LinkContextValue | null>(null);

function createController(): LinkController {
  return new LinkController({
    store: { listHosts, saveHost, forgetHost },
    random,
    clientName: Device.deviceName || Device.modelName || 'Android phone',
    appVersion: Constants.expoConfig?.version ?? '1.0.0',
    isOnline: async () => (await Network.getNetworkStateAsync()).isConnected !== false,
    onNetwork: (cb) => {
      const sub = Network.addNetworkStateListener((s) => cb(s.isConnected !== false));
      return () => sub.remove();
    },
    onForeground: (cb) => {
      const sub = AppState.addEventListener('change', (s) => {
        if (s === 'active') cb();
      });
      return () => sub.remove();
    },
  });
}

export function LinkProvider({ children }: { children: ReactNode }) {
  const [controller] = useState(createController);
  useEffect(() => {
    void controller.start();
    return () => controller.dispose();
  }, [controller]);
  const s = useSyncExternalStore(controller.subscribe, controller.getState);
  const value = useMemo<LinkContextValue>(
    () => ({
      phase: s.phase,
      host: s.host,
      sessions: s.data.sessions,
      inboxCount: s.data.inbox.filter((i) => !i.resolved && !i.expired).length,
      error: s.error,
      scopes: s.data.scopes,
      pairStep: s.pairStep,
      pairFingerprint: s.pairFingerprint,
      retryAt: s.retryAt,
      pair: (offer) => controller.pair(offer),
      cancelPair: () => controller.cancelPair(),
      retryNow: () => controller.retryNow(),
      forget: () => controller.forget(),
    }),
    [controller, s],
  );
  return <LinkContext.Provider value={value}>{children}</LinkContext.Provider>;
}

export function useLink(): LinkContextValue {
  const value = useContext(LinkContext);
  if (!value) throw new Error('useLink needs a LinkProvider');
  return value;
}
