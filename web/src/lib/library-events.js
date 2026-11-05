import { writable } from 'svelte/store';

/**
 * 'The set of songs on disk just changed.'
 *
 * Screens are never destroyed on tab switch — App.svelte toggles visibility so
 * each tab keeps its scroll position and in-flight requests. That is good for
 * the player and fatal for freshness: Home runs `onMount(load)` exactly once per
 * session, so a track downloaded in Search or deleted from a sheet does not show
 * up on Home until the app is restarted. Navigating to the tab is NOT enough,
 * which is the part that looks like a bug to whoever is holding the phone.
 *
 * A counter rather than a boolean, so two changes in a row are two events and
 * `$libraryChanged` is always a fresh value for a reactive statement to fire on.
 */
export const libraryChanged = writable(0);

/** Call after anything that adds or removes a file: download indexed, delete. */
export const noteLibraryChanged = () => libraryChanged.update((n) => n + 1);
