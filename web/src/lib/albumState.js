/**
 * What state is an album in, given the live download jobs for its tracks?
 *
 * One implementation, because there are two places on screen that answer this -
 * the album card in the results grid and the sheet that opens from it - and if
 * they disagree you get a card wearing a tick sitting over a sheet offering
 * "Download all 12". They did disagree, briefly, which is why this is a module
 * and not two copies of an inline expression.
 *
 * DERIVED, NEVER LATCHED. The bug this replaces was a `queued` boolean set the
 * moment the last POST returned. It was never updated again, so a track that had
 * finished downloading still read "queued" forever, and an album whose downloads
 * had failed still wore a tick. Anything computed from the jobs cannot drift,
 * because there is no second copy of the truth to drift from.
 */

/** The job for each of an album's tracks; `undefined` where none was started. */
export const albumJobs = (album, trackJobs = {}) =>
  (album?.tracks || []).map((tr) => trackJobs[tr.sourceId]);

/**
 * Every track has finished.
 *
 * Requires a non-empty track list, so a search result that arrived without one
 * reads as "not done" rather than "done". That asymmetry is deliberate: claiming
 * an album is downloaded when it is not sends someone offline without it.
 */
export function isAlbumDone(album, trackJobs = {}) {
  const jobs = albumJobs(album, trackJobs);
  return jobs.length > 0 && jobs.every((j) => j?.state === 'done');
}

/**
 * Something is still happening.
 *
 * A failed job is not busy - it is finished, unsuccessfully, and the button must
 * become pressable again so the track can be retried.
 */
export function isAlbumBusy(album, trackJobs = {}) {
  return albumJobs(album, trackJobs).some(
    (j) => j && j.state !== 'done' && j.state !== 'failed',
  );
}
