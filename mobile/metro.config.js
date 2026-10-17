// Learn more https://docs.expo.dev/guides/customizing-metro
const path = require('path');
const { getDefaultConfig } = require('expo/metro-config');

const config = getDefaultConfig(__dirname);

// protocol/ sits beside mobile/ and is imported by relative path (no workspace, no copy
// step). Expo watches only this folder outside a workspace, and Metro will not resolve a
// file outside its watched roots, so name it.
config.watchFolders = [...(config.watchFolders ?? []), path.resolve(__dirname, '../protocol')];

module.exports = config;
