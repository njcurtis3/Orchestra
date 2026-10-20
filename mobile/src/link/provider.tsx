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

import type { Answer, InboxItem } from '../../../protocol/types';
import type { LinkFailure } from './client';
import { LinkController } from './controller';
import type { ControllerPhase, HostSummary, PairOutcome } from './controller';
import type { PairingOffer } from './offer';
import type { Outbox } from './outbox';
import { random } from './random';
import type { LinkData } from './state';
import { forgetHost, listHosts, saveHost } from './store';

export type LinkContextValue = {
  phase: ControllerPhase;
  host: HostSummary | null;
  sessions: LinkData['sessions'];
  // Every inbox item as held, whatever its kind and state.
  inbox: InboxItem[];
  // Answers the phone holds, by item id. In memory only.
  outbox: Outbox;
  // Pending permission and question items: not resolved and not expired. What the phone lists.
  inboxCount: number;
  // This phone may answer: the host granted the answer scope.
  canAnswer: boolean;
  error: LinkFailure | null;
  scopes: string[];
  pairStep: 'connecting' | 'confirm' | null;
  pairFingerprint: string | null;
  retryAt: number | null;
  pair: (offer: PairingOffer) => Promise<PairOutcome>;
  cancelPair: () => void;
  retryNow: () => void;
  forget: () => Promise<void>;
  answer: (a: Extract<Answer, { kind: 'permission' | 'question' }>) => void;
  cancelAnswer: (itemId: string) => void;
  retryAnswer: (itemId: string) => void;
  dismissAnswer: (itemId: string) => void;
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
      inbox: s.data.inbox,
      outbox: s.outbox,
      inboxCount: s.data.inbox.filter(
        (i) => (i.kind === 'permission' || i.kind === 'question') && !i.resolved && !i.expired,
      ).length,
      canAnswer: s.data.scopes.includes('answer'),
      error: s.error,
      scopes: s.data.scopes,
      pairStep: s.pairStep,
      pairFingerprint: s.pairFingerprint,
      retryAt: s.retryAt,
      pair: (offer) => controller.pair(offer),
      cancelPair: () => controller.cancelPair(),
      retryNow: () => controller.retryNow(),
      forget: () => controller.forget(),
      answer: (a) => controller.answer(a),
      cancelAnswer: (id) => controller.cancelAnswer(id),
      retryAnswer: (id) => controller.retryAnswer(id),
      dismissAnswer: (id) => controller.dismissAnswer(id),
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
