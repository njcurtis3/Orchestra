// Phone copy (G10). The strings are the approved wording; a trailing [Label] marks a button, which
// the screens split off with splitActions. <name> marks a value filled in with fill.
export const copy = {
  not_paired_title: 'Pair with your desktop',
  not_paired_body:
    'This phone shows what your agents are doing on your computer. Pair it with the Orchestra desktop app first.',
  not_paired_hint: 'On the desktop, open Devices and choose Pair a phone.',
  not_paired_button: 'Pair with a desktop',
  rationale_title: 'Scan the code on your desktop',
  rationale_body:
    'Orchestra uses the camera only to read the pairing code shown by the desktop app. Nothing is recorded or sent anywhere.',
  rationale_button: 'Continue',
  denied_retry: 'Orchestra needs the camera to read the pairing code. [Try again]',
  denied_settings:
    'Camera access is off. To scan the pairing code, allow camera access for Orchestra in Settings. [Open settings]',
  scanning: 'Point the camera at the code on your desktop.',
  bad_code: 'That is not an Orchestra pairing code.',
  newer_code: 'This code is from a newer version of Orchestra. Update this app, then scan again.',
  expired_code:
    'This code has expired. On the desktop, choose Pair a phone again and scan the new code.',
  loopback_only:
    'The desktop is listening only on itself. On the desktop, choose a local network or Tailscale address, then show a new code.',
  connecting: 'Connecting to <host>...',
  confirm_title: 'Confirm on your desktop',
  confirm_body:
    'Your desktop is asking whether to allow this phone. Check that it shows this code: <fingerprint>',
  confirm_wait: 'Waiting for the desktop (<m:ss> left). [Cancel]',
  paired: 'Paired with <host>.',
  pair_window_closed:
    'That code was already used or has closed. On the desktop, choose Pair a phone and scan the new code.',
  pair_auth_failed:
    'This code did not work with the desktop at that address, or the connection dropped. Show a new code on the desktop and scan it again.',
  pair_not_confirmed:
    'Pairing did not finish. The desktop did not accept this phone, or it ran out of time. On the desktop, choose Pair a phone and try again. [Scan again]',
  pair_unreachable:
    'Cannot reach <host> at <addr>. Check that this phone is on the same network as the desktop, or on Tailscale, and that the desktop link is on.',
  offline: 'This phone is offline. Orchestra needs a network connection to reach your desktop.',
  reconnecting: 'Reconnecting to <host>...',
  unreachable:
    'Cannot reach <host>. It may be asleep, closed, or on a different network. Trying again in <n> s. [Try now]',
  version_mismatch:
    'This app and <host> speak different protocol versions. The desktop supports version <supported>; this app speaks version 1. Update whichever is older.',
  not_paired_with_host:
    'Not paired with this host. The desktop no longer recognises this phone. [Pair again] [Forget this desktop]',
  revoked: 'Removed from <host>. Pair again.',
  no_read_scope:
    '<host> has not allowed this phone to see sessions. Change it in Devices on the desktop.',
  no_sessions: 'No sessions on <host> right now.',
  inbox_title: 'Needs you',
  inbox_empty: 'Nothing needs you right now.',
  kind_permission: 'Permission',
  kind_question: 'Question',
  row_label: '<kind> from <project>',
  allow: 'Allow',
  deny: 'Deny',
  deny_message: 'Deny message (optional)',
  other: 'Other',
  send_answers: 'Send answers',
  back: 'Back',
  sending: 'Sending...',
  queued: 'Queued. It sends when <host> is back. Closing the app drops it. [Cancel]',
  unconfirmed: 'No reply from <host> yet. [Try again] [Cancel]',
  sent: 'Sent.',
  failed: 'Not sent: <reason>. Your answer is kept. [Try again] [Cancel]',
  answered: 'This request was already answered. [Dismiss]',
  expired:
    'This request expired when Orchestra closed and can no longer be answered. [Dismiss]',
  gone: 'This request is no longer on <host>. [Dismiss]',
  no_answer_scope:
    '<host> has not allowed this phone to answer. Change it in Devices on the desktop.',
  answer_too_long: 'This answer is too long to send. Keep each answer under 4096 bytes.',
  deny_too_long: 'This message is too long to send.',
  too_many_questions: 'This question has more parts than Orchestra can send.',
  no_questions:
    'This question has nothing to choose from. It stays open until the session ends.',
} as const;

// Wording the approved copy does not cover: the home header, and what to call an unnamed host.
export const extra = {
  need_you: (n: number) => `${n} need you`,
  unknown_host: 'your desktop',
  unknown_project: 'Unknown project',
  row_status: {
    pending: 'Waiting for you',
    queued: 'Queued',
    sending: 'Sending',
    unconfirmed: 'No reply yet',
    sent: 'Sent',
    failed: 'Not sent',
    refused: 'Not sent',
    answered: 'Already answered',
    expired: 'Expired',
    gone: 'Gone',
  },
  tool_label: 'Tool',
  input_label: 'Input',
  missing_reason: 'the desktop did not accept it',
} as const;

const statusLabels: Readonly<Record<string, string>> = {
  starting: 'Starting',
  working: 'Working',
  waiting_permission: 'Needs permission',
  waiting_question: 'Has a question',
  needs_attention: 'Needs you',
  idle: 'Idle',
  suspended: 'Suspended',
  ended: 'Ended',
  failed: 'Failed',
};

// An unknown status is shown as the host sent it.
export function statusLabel(status: string): string {
  return Object.prototype.hasOwnProperty.call(statusLabels, status) ? statusLabels[status] : status;
}

export function fill(template: string, values: Record<string, string | number>): string {
  return template.replace(/<([^>]+)>/g, (whole, key: string) =>
    key in values ? String(values[key]) : whole,
  );
}

// 'Cannot reach it. [Try now]' becomes the sentence and the button labels, in order.
export function splitActions(template: string): { text: string; actions: string[] } {
  const actions: string[] = [];
  const text = template.replace(/\s*\[([^\]]+)\]/g, (_whole, label: string) => {
    actions.push(label);
    return '';
  });
  return { text: text.trim(), actions };
}

export function countdown(msLeft: number): string {
  const total = Math.max(0, Math.ceil(msLeft / 1000));
  const s = total % 60;
  return `${Math.floor(total / 60)}:${s < 10 ? '0' : ''}${s}`;
}
