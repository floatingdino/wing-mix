# Fixtures

Local-only (git-ignored; the repo is public). Present on the coordinator's machine at
`/Users/sam/code/wing-mix/fixtures/` — worktrees do not contain them, so reference that absolute path.

- `nbhd-sunday.RPP`: REAPER session, 44.1 kHz project rate, 28 items all at position 0, length 5720 s (~95 min).
- `Media/*.wav`: 28 mono 24-bit 48 kHz multitrack WAVs (live recording, WING channel numbering in file names:
  03 Kick, 04 Snare, 05 Hats, 06–08 Toms, 09/10 OH, 19/20/30/31 TX, 21 BASS, 23 BASS DRY, 22 AC, 35 MD,
  38–40 VOX 1–3, unnamed others). **Music starts around 3:00**; the first ~180 s are pre-show.
- Never commit, copy into the repo, or upload these files. Tests that use them must skip when the folder is absent
  (`WINGMIX_FIXTURES` env var overrides the path) and be `#[ignore = "slow: ..."]` unless they read only a short excerpt.

## Loader and representative windows (M014)
Code: `crates/mix-fixtures` (RPP parser, seek-based excerpt reader, loudness scan; `mix_fixtures::WINDOWS`).
CLI: `wing-mix-cli fixture info | excerpt --start 180 --dur 30 [--out DIR] | scan [--step 10]` (excerpts default to the
OS temp dir; a 30 s x 28 track excerpt reads in ~0.15 s release). `WINGMIX_FIXTURES` overrides the folder.

Project facts: 56 REAPER tracks = 28 audio tracks (one item each, position 0, no source offset) plus folder buses
(Band, Drums, ...). Item/track gains in the RPP are the engineer's trims (e.g. kick +9.6 dB); excerpts are the raw
recorded signal, gains are reported separately (`AudioTrack::project_gain`). ch 30 (TX) and ch 35 (MD) are muted in
the project. Music actually starts at ~190 s (pre-show noise floor is about -60 dBFS summed).

Levels below are summed mean-square of the raw tracks, dBFS (total / vocals ch 38-40 / drums ch 3-10):

| name | window | total / vocals / drums | character |
|---|---|---|---|
| quiet-verse | 1500-1530 s | -25 / -35 / -49 | sparse verse, drums very light, vocal present |
| full-band | 3900-3930 s | -14 / -21 / -28 | whole band, loudest sustained part of the show |
| vocal-forward | 3420-3450 s | -15 / -19 / -28 | full band with the strongest vocal-to-total ratio |
| no-vocals | 4080-4110 s | -16 / -55 / -51 | instrumental bed, vocal channels are bleed only |

## Reference mixes (`mixes/`)
Commercial recordings supplied by the user as reference mixes (local only, copyrighted — never commit, upload, or
quote audio). All AAC in .m4a, 44.1 kHz stereo:
- `Holy Forever.m4a` (644 s), `I Know That I Know.m4a` (319 s), `Show me Your Face.m4a` (497 s).
These are likely songs in the multitrack set (a Sunday worship set); M016 aligns them to multitrack time ranges.
