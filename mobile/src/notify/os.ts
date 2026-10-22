// The one file that imports expo-notifications. In memory only: nothing is written to disk or logged.
import * as Notifications from 'expo-notifications';

import { parseTap, type NotifyPayload } from './payload';

export const CHANNEL_ID = 'needs-you';

export type AlertStatus = { status: 'undetermined' | 'granted' | 'denied'; canAskAgain: boolean };

export async function ensureChannel(): Promise<void> {
  await Notifications.setNotificationChannelAsync(CHANNEL_ID, {
    name: 'Needs you',
    description: 'An agent is asking for permission or has a question.',
    importance: Notifications.AndroidImportance.HIGH,
    showBadge: false,
    // A secure lock screen shows only the app name, not the contents.
    lockscreenVisibility: Notifications.AndroidNotificationVisibility.PRIVATE,
  });
}

function toStatus(r: { status: string; canAskAgain: boolean }): AlertStatus {
  const status = r.status === 'granted' ? 'granted' : r.status === 'denied' ? 'denied' : 'undetermined';
  return { status, canAskAgain: r.canAskAgain };
}

export async function getStatus(): Promise<AlertStatus> {
  return toStatus(await Notifications.getPermissionsAsync());
}

// The channel goes first: Android 13 shows no prompt until one exists.
export async function request(): Promise<AlertStatus> {
  await ensureChannel();
  return toStatus(await Notifications.requestPermissionsAsync());
}

export async function post(p: NotifyPayload): Promise<void> {
  // The channel carries the lock-screen privacy; on Android 8-12 request() never runs, so ensure it here.
  await ensureChannel();
  await Notifications.scheduleNotificationAsync({
    identifier: p.identifier,
    content: { title: p.content.title, body: p.content.body, data: p.content.data },
    trigger: { channelId: CHANNEL_ID },
  });
}

export async function dismiss(id: string): Promise<void> {
  await Notifications.dismissNotificationAsync(id);
}

export async function dismissAll(): Promise<void> {
  await Notifications.dismissAllNotificationsAsync();
}

// A tap while the app runs. Returns the unsubscribe. Android keeps every response as the last one,
// so it is cleared first: a remounted root must not replay it as a cold start.
export function onTap(cb: (data: unknown) => void): () => void {
  const sub = Notifications.addNotificationResponseReceivedListener((r) => {
    Notifications.clearLastNotificationResponse();
    cb(r.notification.request.content.data);
  });
  return () => sub.remove();
}

// The tap that launched the app, if any. Cleared so it is handled once.
export function takeLaunchTap(): { data: unknown } | null {
  const r = Notifications.getLastNotificationResponse();
  if (!r) return null;
  Notifications.clearLastNotificationResponse();
  return { data: r.notification.request.content.data };
}

// Where a tap goes: a valid id opens that item (a gone one shows the gone screen), anything else goes home.
export function tapRoute(data: unknown): { pathname: '/inbox/[id]'; params: { id: string } } | null {
  const id = parseTap(data);
  return id ? { pathname: '/inbox/[id]', params: { id } } : null;
}

// Banner and list shown, sound on, no badge.
export function setHandler(): void {
  Notifications.setNotificationHandler({
    handleNotification: async () => ({
      shouldShowBanner: true,
      shouldShowList: true,
      shouldPlaySound: true,
      shouldSetBadge: false,
    }),
  });
}
