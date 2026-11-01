import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import { execSync } from 'child_process';
import pkg from './package.json';

const getGitCommitHash = (): string => {
  try {
    return execSync('git rev-parse --short HEAD', { encoding: 'utf-8' }).trim();
  } catch {
    return process.env.VITE_GIT_COMMIT || '';
  }
};

const gitHash = getGitCommitHash();
const appVersion = gitHash ? `${pkg.version}-${gitHash}` : pkg.version;

export default defineConfig({
  plugins: [react()],
  define: {
    __APP_VERSION__: JSON.stringify(appVersion),
    __RAW_VERSION__: JSON.stringify(pkg.version),
    __GIT_COMMIT__: JSON.stringify(gitHash),
    __BUILD_TIME__: JSON.stringify(buildStamp),
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      ignored: ['**/src-tauri/**'],
    },
  },
  envPrefix: ['VITE_', 'TAURI_'],
  build: {
    target: process.env.TAURI_PLATFORM === 'windows' ? 'chrome105' : 'safari13',
    minify: !process.env.TAURI_DEBUG ? 'esbuild' : false,
    sourcemap: !!process.env.TAURI_DEBUG,
  },
});
