# Orchestra mobile

Expo (React Native) companion app for Orchestra, Android first. On its own it does nothing:
it needs a paired Orchestra host (the desktop app, or a server running orchestrad), and
pairing is not built yet. The home screen says so.

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
