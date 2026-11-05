import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { VitePWA } from 'vite-plugin-pwa';

// Dev-only. In production everything is same-origin because the Go binary serves
// both the API and this bundle, so no host appears anywhere in src/.
const devAPI = process.env.MUSIC_HUB_API || 'http://127.0.0.1:8080';

// Compiled into the bundle so a browser can be asked what it is running. Without
// this the only handle on "which build is this tab" was the asset hash in the
// server's request log, compared by hand.
const clientVersion = process.env.MH_VERSION || 'dev';
const clientCommit = process.env.MH_COMMIT || 'unknown';

export default defineConfig({
  plugins: [
    svelte(),
    VitePWA({
      registerType: 'autoUpdate',
      injectRegister: 'script-defer',
      includeAssets: ['icon.svg', 'icon-192.png', 'icon-512.png'],
      manifest: {
        id: '/',
        name: 'Music Hub',
        short_name: 'Music',
        description: 'Your library, search-and-download, and a swipe deck for new music.',
        start_url: '/',
        scope: '/',
        display: 'standalone',
        orientation: 'portrait',
        background_color: '#020617',
        theme_color: '#020617',
        categories: ['music', 'entertainment'],
        icons: [
          { src: '/icon-192.png', sizes: '192x192', type: 'image/png', purpose: 'any' },
          { src: '/icon-512.png', sizes: '512x512', type: 'image/png', purpose: 'any' },
          {
            src: '/icon-192-maskable.png',
            sizes: '192x192',
            type: 'image/png',
            purpose: 'maskable',
          },
          {
            src: '/icon-512-maskable.png',
            sizes: '512x512',
            type: 'image/png',
            purpose: 'maskable',
          },
        ],
      },
      workbox: {
        globPatterns: ['**/*.{js,css,html,svg,png,woff2}'],
        cleanupOutdatedCaches: true,
        clientsClaim: true,
        skipWaiting: true,
        navigateFallback: '/index.html',
        // The OIDC round-trip and every API call must reach the origin. Serving
        // the app shell for /auth/callback would silently break sign-in.
        navigateFallbackDenylist: [/^\/api\//, /^\/auth\//, /^\/healthz$/],
        runtimeCaching: [
          {
            // Cover art is immutable per id and is the only thing worth holding
            // on to: it makes the library usable on a bad connection.
            urlPattern: ({ url, sameOrigin }) =>
              sameOrigin && url.pathname.startsWith('/api/cover/'),
            handler: 'CacheFirst',
            options: {
              cacheName: 'music-hub-cover',
              expiration: { maxEntries: 400, maxAgeSeconds: 60 * 60 * 24 * 30 },
              cacheableResponse: { statuses: [0, 200] },
            },
          },
          {
            // Everything else under /api/ is either private, live, or huge.
            //
            // Three paths are deliberately NOT matched, because an unrouted
            // request never enters the service worker's fetch handler at all:
            //
            //   /api/stream/            audio - keeps Range/seek behaviour
            //                           identical to having no worker installed
            //   /api/discovery/preview/ same, for clips
            //   /api/playback/stream    the SSE event stream
            //
            // The last one was missing until 2026-08-25 and cost a day of
            // "the connection keeps dropping". Chrome terminates an idle service
            // worker after ~30 seconds and takes any response it is holding with
            // it, so an infinite stream routed through the worker died on a
            // 30-second cycle - measured at 30.05s, forty times in twenty-four
            // minutes. Every reconnect leaves a gap where this tab's version is
            // stale, which is what made skipping a track feel broken.
            //
            // Note the near-miss: the audio exclusion is `/api/stream/` and the
            // event stream is `/api/playback/stream`. The prefix that looked like
            // it covered both covered only one.
            urlPattern: ({ url, sameOrigin }) =>
              sameOrigin &&
              url.pathname.startsWith('/api/') &&
              !url.pathname.startsWith('/api/stream/') &&
              !url.pathname.startsWith('/api/discovery/preview/') &&
              url.pathname !== '/api/playback/stream',
            handler: 'NetworkOnly',
          },
        ],
      },
    }),
  ],
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    target: 'es2020',
  },
  define: {
    __MH_VERSION__: JSON.stringify(clientVersion),
    __MH_COMMIT__: JSON.stringify(clientCommit),
  },
  server: {
    port: 3000,
    proxy: {
      '/api': devAPI,
      '/auth': devAPI,
      '/healthz': devAPI,
    },
  },
});
