import { writable } from 'svelte/store';

/**
 * Display rules for a track.
 *
 * 428 of 508 tracks in this library are the only track on their album, and
 * Navidrome captions those "[Unknown Album]". Repeating that under every row is
 * noise pretending to be information: a single has no album, and saying so
 * badly is worse than saying nothing.
 */

// Navidrome's own placeholder, plus what downloaders leave behind.
const PLACEHOLDER = /^\s*(\[?\s*unknown\s*album\s*\]?|unknown|untitled|single|singles|n\/?a|-{1,3})\s*$/i;

/** True when an album name carries no information worth showing. */
export function isPlaceholderAlbum(album, title = '') {
  if (!album) return true;
  const a = album.trim();
  if (!a) return true;
  if (PLACEHOLDER.test(a)) return true;
  // yt-dlp commonly sets album = title for a single; showing "X · X" is noise.
  return a.toLowerCase() === title.trim().toLowerCase();
}

/** "Artist · Album", or just the artist when the album says nothing. */
export function subtitleFor(track) {
  const artist = track.artist || 'Unknown artist';
  return isPlaceholderAlbum(track.album, track.title) ? artist : `${artist} · ${track.album}`;
}

/**
 * Tracks deleted in this session.
 *
 * The same song shows up in several lists at once - it can be in Recently
 * added, Most played and a mix on one screen - and each list holds its own
 * copy. Without a shared record, deleting a file leaves its ghost in the other
 * lists until a reload, and tapping the ghost 404s. Navidrome also keeps
 * serving the row until its rescan finishes, so this cannot wait on the server.
 */
export const removedIds = writable(new Set());

export function markRemoved(id) {
  removedIds.update((s) => new Set(s).add(id));
}
