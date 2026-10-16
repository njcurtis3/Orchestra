import { render, screen } from '@testing-library/react-native';
import { SafeAreaProvider } from 'react-native-safe-area-context';

import HomeScreen from '@/app/index';

const metrics = {
  frame: { x: 0, y: 0, width: 360, height: 800 },
  insets: { top: 24, left: 0, right: 0, bottom: 16 },
};

describe('HomeScreen', () => {
  it('says the phone needs a paired host', async () => {
    await render(
      <SafeAreaProvider initialMetrics={metrics}>
        <HomeScreen />
      </SafeAreaProvider>,
    );
    expect(screen.getByText('Orchestra needs a host')).toBeTruthy();
    expect(screen.getByText(/does nothing on its own/)).toBeTruthy();
    expect(screen.getByText(/orchestrad/)).toBeTruthy();
    expect(screen.getByText(/Pairing arrives in a later version/)).toBeTruthy();
  });

  it('shows no session or list content', async () => {
    await render(
      <SafeAreaProvider initialMetrics={metrics}>
        <HomeScreen />
      </SafeAreaProvider>,
    );
    expect(screen.queryByRole('button')).toBeNull();
    expect(screen.queryByText(/session/i)).toBeNull();
  });
});
