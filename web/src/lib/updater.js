/**
 * In-app updates for the Android build.
 *
 * Releases are published on GitHub. The newest release is read from the public
 * GitHub API and its `.apk` asset is handed to the native side, which downloads
 * it and opens the system installer.
 *
 * Everything here is a no-op in a browser: a PWA updates itself, and there is
 * nothing to install.
 */

import { Capacitor, registerPlugin } from '@capacitor/core';
import { App } from '@capacitor/app';

const Updater = registerPlugin('Updater');

/** Where the newest Android release is looked up. */
export const RELEASES_API = 'https://api.github.com/repos/y0c0ms/Music-Hub/releases/latest';

export const isNative = Capacitor.isNativePlatform();

/**
 * Whether this build can install an update by itself.
 *
 * The web app is served live while the APK is not, so a phone running an older
 * build receives this screen the moment it ships and before it has the native
 * plugin to back it. Without this check the button appears, is pressed, and
 * fails with "plugin not available" - which is exactly what happened. An older
 * build is offered the download link instead.
 */
export const canSelfUpdate = isNative && Capacitor.isPluginAvailable('Updater');

/** The version currently installed, or null in a browser. */
export async function installedVersion() {
  if (!isNative) return null;
  try {
    const info = await App.getInfo();
    return info?.version ?? null;
  } catch {
    return null;
  }
}

/**
 * Compare dotted versions numerically.
 *
 * A string compare says 1.0.9 is newer than 1.0.10, and this project reached
 * 1.0.4 in a single evening - it will get there.
 */
export function isNewer(a, b) {
  if (!a || !b) return false;
  const pa = String(a).replace(/^v/, '').split('.').map((n) => parseInt(n, 10) || 0);
  const pb = String(b).replace(/^v/, '').split('.').map((n) => parseInt(n, 10) || 0);
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const x = pa[i] ?? 0;
    const y = pb[i] ?? 0;
    if (x !== y) return x > y;
  }
  return false;
}

/**
 * The newest published Android release.
 *
 * Resolves `{ready:false}` rather than throwing when there is no release or no
 * `.apk` asset on it, so the Settings screen can simply not show the section.
 */
export async function latestRelease() {
  const res = await fetch(RELEASES_API, {
    headers: { Accept: 'application/vnd.github+json' },
  });
  if (!res.ok) return { ready: false };
  const rel = await res.json();
  const apk = (rel?.assets ?? []).find((a) => a?.name?.endsWith('.apk'));
  if (!apk?.browser_download_url) return { ready: false };
  return {
    ready: true,
    version: String(rel.tag_name ?? '').replace(/^v/, ''),
    size: apk.size ?? 0,
    url: apk.browser_download_url,
  };
}

/**
 * Hand the download to the native side, which fetches it into the app's private
 * cache and opens the system installer. Android still asks the user to confirm,
 * and needs "install unknown apps" allowed for this app once.
 */
export async function installUpdate(url) {
  if (!isNative) throw new Error('Updates only apply to the Android app');
  return Updater.downloadAndInstall({ url });
}

const ServerConfig = registerPlugin('ServerConfig');

/** Whether this build lets the user pick a different server from Settings. */
export const canChangeServer = isNative && Capacitor.isPluginAvailable('ServerConfig');

/** Forget the configured server; the app restarts into its setup page. */
export function changeServer() {
  return ServerConfig.reset();
}
