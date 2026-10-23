# Orchestra mobile

Expo (React Native) companion app for Orchestra, Android first. On its own it does nothing:
it needs a paired Orchestra host (the desktop app, or a server running orchestrad), which
you pair with by scanning its QR code.

```bash
npm ci
npx expo start   # also generates the gitignored expo-env.d.ts
npm test
npm run lint
npx tsc --noEmit
```

`ios/` and `android/` are generated (Continuous Native Generation); configure native
behavior in `app.json`. See `AGENTS.md` and `../CLAUDE.md` for the rules.

`LICENSE` stays because some files descend from the Expo project template
(`themed-text.tsx`, `themed-view.tsx`, the hooks, `constants/theme.ts`).

## Build and install (Android preview)

The preview profile builds an installable APK on EAS. These steps are the owner's: they use
your Expo login and upload the source, so they are not run by agents. Run them in `mobile/`.

```bash
npm ci
npm install -g eas-cli          # eas.json needs cli >= 16.0.0
eas login                       # personal njcurtis3 Expo account
eas init                        # writes owner and extra.eas.projectId into app.json
git commit -am "mobile: link EAS project"
eas build --platform android --profile preview
```

- The first build asks to generate an Android keystore. Answer yes: EAS stores it
  server-side. To keep a backup, run `eas credentials --platform android` and save the
  keystore in a password manager, never in the repo.
- Sideload: open the build URL or QR code from the build output on the phone, download the
  APK, allow install from unknown apps for that browser, and install.
- The app talks plain `ws://` to a desktop on the LAN or a Tailscale IP. Release builds
  allow cleartext on purpose (see `../CLAUDE.md`). There is no internet service and no
  OTA update: `expo-updates` is not installed, so a change ships as a new build.
- Each preview build raises the version code (`autoIncrement`), so a new APK installs over
  the last one without an uninstall and keeps the pairing keys.
