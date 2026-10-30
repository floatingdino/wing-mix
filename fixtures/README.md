# Fixtures

Local-only (git-ignored; the repo is public). Present on the coordinator's machine at
`/Users/sam/code/wing-mix/fixtures/` — worktrees do not contain them, so reference that absolute path.

- `nbhd-sunday.RPP`: REAPER session, 44.1 kHz project rate, 28 items all at position 0, length 5720 s (~95 min).
- `Media/*.wav`: 28 mono 24-bit 48 kHz multitrack WAVs (live recording, WING channel numbering in file names:
  03 Kick, 04 Snare, 05 Hats, 06–08 Toms, 09/10 OH, 19/20/30/31 TX, 21 BASS, 23 BASS DRY, 22 AC, 35 MD,
  38–40 VOX 1–3, unnamed others). **Music starts around 3:00**; the first ~180 s are pre-show.
- Never commit, copy into the repo, or upload these files. Tests that use them must skip when the folder is absent
  (`WINGMIX_FIXTURES` env var overrides the path) and be `#[ignore = "slow: ..."]` unless they read only a short excerpt.
