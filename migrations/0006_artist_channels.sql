-- Resolved YouTube channel per artist, for the New Releases YouTube source.
--
-- Channel resolution is the expensive part of the YouTube sweep: a channel is
-- only trusted when a known song the user already has actually appears on it, or
-- when the channel name matches the artist exactly. Both cost a search, so the
-- answer is cached here permanently. An artist whose channel moves is rare, and
-- `DELETE`-ing the row re-resolves it on the next sweep.
--
-- `name` and `confirmed_by` record why the channel was accepted: 'song' means a
-- known track the user owns was found there, 'name' means only the channel name
-- matched. A 'name' row is weaker evidence and is worth re-checking.
CREATE TABLE IF NOT EXISTS artist_channels (
    artist_key  text PRIMARY KEY,
    channel_id  text NOT NULL,
    channel_name text NOT NULL DEFAULT '',
    confirmed_by text NOT NULL DEFAULT '',
    resolved_at timestamptz NOT NULL DEFAULT now()
);
