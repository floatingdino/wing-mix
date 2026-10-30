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

## Non-input tracks (confirmed by the user, 2026-10-30)
- `43`: empty (silent) — ignore.
- `52` / `53`: **FOH L/R** — the engineer's actual live main mix as recorded. Not console inputs: use as the
  "human mix" baseline/evaluation tap (and for aligning reference songs to the set).
- `55` / `56`: **video mix L/R** — a second, separate mix (broadcast/stream). Not console inputs; secondary baseline.
Do not patch these into console input channels.

## Reference mixes (`mixes/`)
Commercial recordings supplied by the user as reference mixes (local only, copyrighted — never commit, upload, or
quote audio). All AAC in .m4a, 44.1 kHz stereo:
- `Holy Forever.m4a` (644 s), `I Know That I Know.m4a` (319 s), `Show me Your Face.m4a` (497 s).
These are likely songs in the multitrack set (a Sunday worship set); M016 aligns them to multitrack time ranges.

## Songs in the multitrack (M016)
Code: `crates/mix-fixtures` (`align`: chroma + block matching / DTW; `songs`: `SONGS`, `matched_pair`). CLI:
`wing-mix-cli fixture align [--cache DIR]` (~10 s release; caches an 8 kHz mono FOH fold-down and the reference
profiles in the OS temp dir, never the repo). Method: the references are matched against the **FOH L/R recording
(52/53)** with 0.1 s chroma; every 30 s block of a record (15 s hop) is tried at every offset in all 12 keys at the
record's tempo, blocks whose best offset is sharply better than its neighbours (prominence) vote for a key + region,
and stretches where several blocks share an offset are what the band played straight along the record. The band
plays to the records' tempo (offsets agree to ~0.1 s; Show Me Your Face drifts ~1.5 s over 45 s) but transposes.
Confidence = (winner score - best other key/region) / winner score, scaled down below 4 anchored blocks.

| song | multitrack range (matched) | live key vs record | confidence |
|---|---|---|---|
| I Know That I Know | 168.9-438.9 s (2:49-7:19) | -2 semitones | 0.98 |
| Holy Forever | 829.2-1157.4 s (13:49-19:17) | -5 (+7) | 0.89 |
| Show Me Your Face | 1245.6-1318.8 s (20:46-21:59) | +5 | 0.70 (short live rendition, ~1:15 matched) |

The live intros/outros may extend a little beyond the matched ranges (e.g. music starts ~150 s). Holy Forever's
album version (10:44) is cut to ~5.5 min live; from chorus 4 on (record 436-644 s) nothing was found. Show Me Your Face
is mostly a repeated verse/chorus around 1250-1320 s; its intro and record 151-229 s were not found. The quieter stretch
1150-1245 s is unidentified.

Sections (mix-reference `detect_sections` on the record; multitrack range = section start/end + segment offset;
sections not listed were not found live; coverage < 100 % means only part of the section matched):

| song | section (record s) | multitrack s | coverage |
|---|---|---|---|
| I Know That I Know | 1 bridge 1 (14.7-27.0) | 168.9-180.9 | 98 % |
| | 2 verse 1 (27.0-54.5) | 180.9-208.4 | 100 % |
| | 3 chorus 1 (54.5-68.7) | 208.4-222.6 | 100 % |
| | 4 verse 2 (68.7-96.2) | 222.6-250.1 | 100 % |
| | 5 verse 3 (96.2-118.7) | 250.1-272.6 | 100 % |
| | 6 verse 4 (118.7-163.9) | 272.6-317.8 | 100 % |
| | 7 verse 5 (163.9-174.7) | 317.8-328.6 | 100 % |
| | 8 verse 6 (174.7-224.2) | 328.6-378.1 | 100 % |
| | 9 chorus 2 (224.2-278.2) | 378.1-432.1 | 100 % |
| Holy Forever | 2 bridge 2 (33.4-59.9) | 832.6-859.1 | 100 % |
| | 3 verse 1 (59.9-76.1) | 859.1-875.3 | 100 % |
| | 4 verse 2 (76.1-86.8) | 875.3-886.0 | 100 % |
| | 5 bridge 3 (86.8-107.5) | 886.0-904.2 | 88 % |
| | 6 bridge 4 (107.5-116.8) | 933.4-942.7 | 100 % |
| | 7 bridge 5 (116.8-139.8) | 942.7-965.7 | 100 % |
| | 8 chorus 1 (139.8-200.2) | 965.7-1026.1 | 100 % |
| | 9 chorus 2 (200.2-307.2) | 872.6-932.6 | 56 % |
| | 10 verse 3 (307.2-316.0) | 1119.6-1128.4 | 100 % |
| | 11 verse 4 (316.0-329.7) | 1128.4-1142.1 | 100 % |
| | 14 chorus 3 (400.4-436.2) | 906.1-940.7 | 97 % |
| Show Me Your Face | 2 bridge 2 (76.5-93.2) | 1247.1-1263.8 | 100 % |
| | 3 bridge 3 (93.2-108.9) | 1263.8-1279.5 | 100 % |
| | 4 verse 1 (108.9-121.7) | 1277.8-1290.6 | 100 % |
| | 5 bridge 4 (121.7-134.4) | 1290.6-1303.3 | 100 % |
| | 6 verse 2 (134.4-151.1) | 1303.3-1318.9 | 93 % |
| | 11 verse 6 (228.7-281.2) | 1272.8-1314.0 | 78 % |
| | 15 outro (453.4-497.0) | 1271.6-1313.2 | 95 % |

Section labels are the analyser's hints, not the song's real form. Tests: `mix_fixtures::songs::played_sections`
lists these at run time and `matched_pair(fx, song, Some(section), cache, max_dur)` returns the multitrack excerpt
plus the reference profile of the same section (for M031/M033/M052).
`mix-describe`'s `worship` genre (aliases `contemporary`, `ccm`) is the average of these 3 references' profiles.
