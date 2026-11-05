import './app.css';
import App from './App.svelte';
import { selfUpdateIfStale } from './lib/appUpdate.js';
import { serverVersion } from './lib/api.js';

// Apply a new build on THIS launch, not the next one.
//
// Reported 2026-08-25: "I keep closing the app and opening but nothing changes,
// and the settings still don't say which version". Both were true, and the cause
// was not the deploy pipeline - the phone had fetched the new bundle. The proof
// was that it had never once called /api/version, an endpoint only the new
// Settings screen requests, while the proxy log showed it downloading that exact
// bundle minutes earlier.
//
// That is how Workbox `autoUpdate` behaves: the service worker installs, takes
// control (skipWaiting + clientsClaim), and PRECACHES the new assets - while the
// page that is already open keeps running the old ones. The new code lands on the
// following launch. With eight deploys in a day the app is then permanently one
// version behind, and closing and reopening genuinely changes nothing, because
// each launch runs what the previous one downloaded.
//
// `controllerchange` fires when a new worker takes over. Reloading there collapses
// the two launches into one.
//
// Two guards, both necessary:
//   - only when a controller ALREADY exists. On a first install the controller
//     goes from null to new, and reloading for that is a pointless flash.
//   - once. A reload can re-fire this, and a service worker that reloads the page
//     in a loop is far worse than a stale bundle.
if ('serviceWorker' in navigator && navigator.serviceWorker.controller) {
  let reloading = false;
  navigator.serviceWorker.addEventListener('controllerchange', () => {
    if (reloading) return;
    reloading = true;
    window.location.reload();
  });
}

// Compare the commit compiled into this bundle against the one the server
// reports, and fix the tab if they differ. This is the check that would have
// caught today's whole mess in seconds: the app ran a stale build for hours
// across repeated restarts while every deploy reported success, and nothing
// compared those two numbers at runtime.
selfUpdateIfStale(__MH_COMMIT__, serverVersion);

const app = new App({
  target: document.getElementById('app'),
});

export default app;
