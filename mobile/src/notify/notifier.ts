// Turns the controller's fresh and gone items into notifications, unless the user is already looking at them.
import { AppState } from 'react-native';

import type { InboxItem, SessionSummary } from '../../../protocol/types';
import * as os from './os';
import { buildPayload } from './payload';

export type InboxEvent = { fresh: InboxItem[]; gone: string[]; reset?: boolean };
type Os = Pick<typeof os, 'post' | 'dismiss' | 'dismissAll'>;

// No alert while the app is open on home (where Needs you is shown) or on that item's own screen.
export function isSuppressed(appState: string, route: string, id: string): boolean {
  return appState === 'active' && (route === '/' || route === `/inbox/${id}`);
}

export function createNotifier(adapter: Os, appState: () => string) {
  let route = '/';
  const swallow = (p: Promise<void>) => p.catch(() => undefined);
  return {
    setRoute(path: string): void {
      route = path;
    },
    onInbox(e: InboxEvent, sessions: SessionSummary[]): void {
      if (e.reset) swallow(adapter.dismissAll());
      for (const id of e.gone) swallow(adapter.dismiss(id));
      for (const item of e.fresh) {
        if (isSuppressed(appState(), route, item.id)) continue;
        const project = sessions.find((s) => s.id === item.session_id)?.project_name;
        swallow(adapter.post(buildPayload(item, project)));
      }
    },
  };
}

export const notifier = createNotifier(os, () => AppState.currentState);
