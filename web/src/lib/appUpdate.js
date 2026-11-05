/**
 * Force this tab onto the deployed build.
 *
 * `location.reload()` is not enough and 2026-08-25 proved it: the user tapped
 * Reload repeatedly, stayed on v51, and only a force-stop of the app landed v52.
 * That is not stubbornness, it is how service workers work - a reload is a
 * navigation, the CURRENT worker answers it from its precache, and it answers
 * with exactly the build the user is trying to leave. The new worker may be
 * installed and even active, but nothing re-fetches the app shell.
 *
 * So the only honest implementation of a button labelled "Reload" is to remove
 * the thing doing the caching first: unregister every worker, drop every cache,
 * then navigate with a cache-busting query so the HTTP layer cannot answer from
 * its own copy either. The worker reinstalls on the fresh load and precaches the
 * new build.
 *
 * Heavy-handed on purpose. This runs when someone has already been told their app
 * is out of date, and a needless refetch of a 280KB bundle is a far better outcome
 * than a button that lies.
 */
export async function hardUpdate() {
  try {
    if ('serviceWorker' in navigator) {
      const regs = await navigator.serviceWorker.getRegistrations();
      await Promise.all(regs.map((r) => r.unregister()));
    }
    if (typeof caches !== 'undefined') {
      const keys = await caches.keys();
      await Promise.all(keys.map((k) => caches.delete(k)));
    }
  } catch {
    // A reload without the cleanup is still better than staying put.
  }
  const url = new URL(window.location.href);
  url.searchParams.set('u', String(Date.now()));
  window.location.replace(url.toString());
}

/**
 * Land on the deployed build without being asked.
 *
 * The commit compiled into this bundle against the one the server reports is an
 * exact answer to "is this tab stale", and it needs no service-worker events to
 * work. Today the app sat one version behind for hours across repeated restarts
 * while every deploy reported success, because nothing compared those two numbers
 * at runtime.
 *
 * Guarded by sessionStorage rather than a variable: hardUpdate() navigates, so a
 * flag in memory would be gone by the time it mattered and a mismatch that
 * survives the update - a bad deploy, a stale CDN copy - would reload forever.
 * One attempt per tab session, then it gives up and lets Settings say so.
 */
const TRIED = 'mh.selfUpdated';

export async function selfUpdateIfStale(clientCommit, fetchServerVersion) {
  if (!clientCommit || clientCommit === 'unknown') return;
  try {
    if (sessionStorage.getItem(TRIED)) return;
  } catch {
    return; // no sessionStorage means no loop protection, so do nothing
  }
  try {
    const server = await fetchServerVersion();
    if (!server?.commit || server.commit === 'unknown') return;
    if (server.commit === clientCommit) return;
    sessionStorage.setItem(TRIED, server.commit);
    await hardUpdate();
  } catch {
    // Offline, or the server is down. Either way this is not the moment.
  }
}
