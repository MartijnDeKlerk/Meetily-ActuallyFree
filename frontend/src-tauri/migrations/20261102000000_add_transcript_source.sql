-- Which capture track a transcript line was transcribed from: 'mic' (the
-- local user) or 'system' (remote participants). NULL for imported audio and
-- for lines saved before this column existed.
--
-- The speaker label used to double as this provenance, but diarization
-- overwrites it, so after one run nothing recorded which track a line came
-- from. Offline diarization reads this column to keep mic lines as "You" and
-- to label system lines only from the system track's own speakers.
ALTER TABLE transcripts ADD COLUMN source TEXT;
