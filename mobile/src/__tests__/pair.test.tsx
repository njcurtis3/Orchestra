import { act, fireEvent, render, screen } from '@testing-library/react-native';
import { Linking } from 'react-native';
import { SafeAreaProvider } from 'react-native-safe-area-context';

import PairScreen from '@/app/pair';
import { copy, fill, splitActions } from '@/constants/copy';
import type { LinkContextValue } from '@/link/provider';

declare const __dirname: string;
const fs = require('fs') as { readFileSync: (path: string, encoding: 'utf8') => string };
const offerFile = JSON.parse(
  fs.readFileSync(`${__dirname}/../../../protocol/fixtures/noise/offer.json`, 'utf8'),
);
const OFFER_TEXT: string = offerFile.json;
const OFFER = JSON.parse(OFFER_TEXT);
const NOW = 1_799_999_000_000;
const FINGERPRINT = 'abcd-1234-ef56-7890';

const mockRouter = { push: jest.fn(), replace: jest.fn(), back: jest.fn() };
let mockLink: LinkContextValue;
let mockPermission: { granted: boolean; status: string; canAskAgain: boolean } | null;
const mockRequest = jest.fn();
let mockCamera: { onBarcodeScanned: (e: { data: string }) => void; barcodeScannerSettings: unknown };

jest.mock('expo-router', () => ({ useRouter: () => mockRouter }));
jest.mock('@/link/provider', () => ({ useLink: () => mockLink }));
jest.mock('expo-camera', () => ({
  useCameraPermissions: () => [mockPermission, mockRequest],
  CameraView: (props: typeof mockCamera) => {
    mockCamera = props;
    return null;
  },
}));

const metrics = {
  frame: { x: 0, y: 0, width: 360, height: 800 },
  insets: { top: 24, left: 0, right: 0, bottom: 16 },
};

function link(over: Partial<LinkContextValue> = {}): LinkContextValue {
  return {
    phase: 'unpaired',
    host: null,
    sessions: [],
    inboxCount: 0,
    error: null,
    scopes: [],
    pairStep: null,
    pairFingerprint: null,
    retryAt: null,
    pair: jest.fn(),
    cancelPair: jest.fn(),
    retryNow: jest.fn(),
    forget: jest.fn(),
    ...over,
  } as LinkContextValue;
}

async function show(over: Partial<LinkContextValue> = {}) {
  mockLink = link(over);
  return render(
    <SafeAreaProvider initialMetrics={metrics}>
      <PairScreen />
    </SafeAreaProvider>,
  );
}

const granted = { granted: true, status: 'granted', canAskAgain: true };

async function toScanner(over: Partial<LinkContextValue> = {}) {
  mockPermission = granted;
  const view = await show(over);
  await fireEvent.press(screen.getByRole('button', { name: copy.rationale_button }));
  return view;
}

async function scan(data: string) {
  await act(async () => {
    mockCamera.onBarcodeScanned({ data });
  });
}

const pending = () => jest.fn(() => new Promise(() => undefined)) as never;

jest.setTimeout(30_000);

beforeEach(() => {
  jest.clearAllMocks();
  jest.spyOn(Date, 'now').mockReturnValue(NOW);
  mockPermission = null;
});
afterEach(() => jest.restoreAllMocks());

describe('PairScreen camera permission', () => {
  it('shows the rationale before requesting the permission', async () => {
    await show();
    expect(screen.getByText(copy.rationale_title)).toBeTruthy();
    expect(screen.getByText(copy.rationale_body)).toBeTruthy();
    expect(mockRequest).not.toHaveBeenCalled();
    await fireEvent.press(screen.getByRole('button', { name: copy.rationale_button }));
    expect(mockRequest).toHaveBeenCalledTimes(1);
  });

  it('denied but askable offers Try again, which asks again', async () => {
    mockPermission = { granted: false, status: 'denied', canAskAgain: true };
    await show();
    await fireEvent.press(screen.getByRole('button', { name: copy.rationale_button }));
    mockRequest.mockClear();
    expect(screen.getByText(splitActions(copy.denied_retry).text)).toBeTruthy();
    await fireEvent.press(screen.getByRole('button', { name: 'Try again' }));
    expect(mockRequest).toHaveBeenCalledTimes(1);
  });

  it('denied for good offers Open settings, which calls Linking.openSettings', async () => {
    const open = jest.spyOn(Linking, 'openSettings').mockResolvedValue(undefined);
    mockPermission = { granted: false, status: 'denied', canAskAgain: false };
    await show();
    await fireEvent.press(screen.getByRole('button', { name: copy.rationale_button }));
    expect(screen.getByText(splitActions(copy.denied_settings).text)).toBeTruthy();
    await fireEvent.press(screen.getByRole('button', { name: 'Open settings' }));
    expect(open).toHaveBeenCalledTimes(1);
  });

  it('granted shows the camera, scanning only QR codes', async () => {
    await toScanner();
    expect(screen.getByText(copy.scanning)).toBeTruthy();
    expect(mockCamera.barcodeScannerSettings).toEqual({ barcodeTypes: ['qr'] });
  });
});

describe('PairScreen scanned codes', () => {
  it('a bad code shows the not-an-Orchestra-code copy and Scan again returns to the camera', async () => {
    await toScanner();
    await scan('https://example.com');
    expect(screen.getByText(copy.bad_code)).toBeTruthy();
    await fireEvent.press(screen.getByRole('button', { name: 'Scan again' }));
    expect(screen.getByText(copy.scanning)).toBeTruthy();
  });

  it('a newer, expired or loopback-only code shows its own copy', async () => {
    await toScanner();
    await scan(JSON.stringify({ ...OFFER, v: 2 }));
    expect(screen.getByText(copy.newer_code)).toBeTruthy();
    await fireEvent.press(screen.getByRole('button', { name: 'Scan again' }));
    await scan(JSON.stringify({ ...OFFER, expires_at: NOW - 3_600_000 }));
    expect(screen.getByText(copy.expired_code)).toBeTruthy();
    await fireEvent.press(screen.getByRole('button', { name: 'Scan again' }));
    await scan(JSON.stringify({ ...OFFER, addrs: ['127.0.0.1:47800'] }));
    expect(screen.getByText(copy.loopback_only)).toBeTruthy();
  });

  it('only the first scan is used', async () => {
    const pair = pending();
    await toScanner({ pair });
    await scan(OFFER_TEXT);
    await scan(OFFER_TEXT);
    expect(pair).toHaveBeenCalledTimes(1);
  });
});

describe('PairScreen pairing steps', () => {
  it('connecting names the host', async () => {
    await toScanner({ pair: pending(), pairStep: 'connecting' });
    await scan(OFFER_TEXT);
    expect(screen.getByText(fill(copy.connecting, { host: OFFER.host_name }))).toBeTruthy();
  });

  it('the confirm wait shows the phone fingerprint and a countdown, and Cancel cancels and goes back', async () => {
    const cancelPair = jest.fn();
    await toScanner({ pair: pending(), pairStep: 'confirm', pairFingerprint: FINGERPRINT, cancelPair });
    await scan(OFFER_TEXT);
    expect(screen.getByText(copy.confirm_title)).toBeTruthy();
    expect(screen.getByText(fill(copy.confirm_body, { fingerprint: FINGERPRINT }))).toBeTruthy();
    expect(screen.getByText('Waiting for the desktop (16:40 left).')).toBeTruthy();
    await fireEvent.press(screen.getByRole('button', { name: 'Cancel' }));
    expect(cancelPair).toHaveBeenCalled();
    expect(mockRouter.back).toHaveBeenCalled();
  });

  it('paired shows the host and replaces the route with home', async () => {
    const pair = jest.fn().mockResolvedValue({ ok: true, host: { host_name: 'Desk' } });
    await toScanner({ pair: pair as never });
    await scan(OFFER_TEXT);
    expect(screen.getByText(fill(copy.paired, { host: 'Desk' }))).toBeTruthy();
    expect(mockRouter.replace).toHaveBeenCalledWith('/');
  });

  it.each([
    [{ kind: 'PairWindowClosed' }, copy.pair_window_closed],
    [{ kind: 'PairAuthFailed' }, copy.pair_auth_failed],
    [{ kind: 'PairNotConfirmed' }, splitActions(copy.pair_not_confirmed).text],
    [
      { kind: 'Unreachable' },
      fill(copy.pair_unreachable, { host: OFFER.host_name, addr: '192.168.1.20:47800' }),
    ],
  ])('failure %j shows its copy and Scan again', async (failure, expected) => {
    const pair = jest.fn().mockResolvedValue({ ok: false, failure });
    await toScanner({ pair: pair as never });
    await scan(OFFER_TEXT);
    expect(screen.getByText(expected)).toBeTruthy();
    await fireEvent.press(screen.getByRole('button', { name: 'Scan again' }));
    expect(screen.getByText(copy.scanning)).toBeTruthy();
  });
});

describe('PairScreen leaving', () => {
  it('unmount calls cancelPair', async () => {
    const cancelPair = jest.fn();
    const view = await show({ cancelPair });
    expect(cancelPair).not.toHaveBeenCalled();
    await view.unmount();
    expect(cancelPair).toHaveBeenCalledTimes(1);
  });
});
