# Mixing knowledge base: rationale and sources (M005)

This document explains the rules encoded in `data/knowledge/*.toml`: what each rule says, why, and where it
comes from. The TOML files hold the data, and each one starts with a `[schema]` table. This document holds the
reasoning. Source keys in square brackets (e.g. [senior2011]) match `workflow.toml [sources]` and the
bibliography at the end. All text is paraphrased; no source text is reproduced.

## 0. Design of the knowledge base

### 0.1 Who consumes it
- **M031 diagnosis** compares measured Features with the TargetProfile. It raises Problems using the
  tolerances in `workflow.toml [targets.reference_matching]` and the conditions in the rules.
- **M032 planner** maps Problems to Actions. It uses the per-source presets and problem lists
  (`sources.toml`), the EQ limits and masking strategies (`eq.toml`), the dynamics presets and rules
  (`dynamics.toml`) and the FX conventions (`fx.toml`). It orders work by `workflow.toml [[phase]]`.
- **M024 description parsing** turns words into TargetProfile deltas through `vocabulary.toml`.
- **M025 channel identification** uses `sources.toml name_hints` and the band expectations in each
  source's `character` and `fundamental_hz` entries.

### 0.2 Three deliberate abstractions
1. **Generic blocks, not console paths.** Actions name `Trim`, `Filter`, `Gate`, `Eq`, `Dyn`, `Send`,
   `Fader`, `Pan` and `FxSlot` (the mix-surface `ProcessingBlock`s from ARCHITECTURE.md). Each block's
   `note` records the WING ranges from wing-simulator `docs/research/models.md` and `signal-flow.md`, so the
   ranges are realistic. Examples: channel EQ is a low band, 4 peaking bands and a high band at ±15 dB with
   Q 0.44–10; the low cut is 20–2000 Hz at 6–24 dB/oct; the WING gate has a −80..0 dB threshold and a
   3–60 dB range; the WING compressor has ratios from 1.1 to 100 and attack of 0–120 ms; the FX rack has
   16 stereo slots used as send FX on buses, with reverbs and delays only in slots 1–8. The adapter clamps
   to the real capabilities.
2. **Thresholds relative to signal statistics.** Engineers set thresholds by ear relative to the
   material ("just catching the loud phrases", "just above the bleed"), not to absolute numbers. The KB
   therefore expresses each threshold as `threshold_rel` (`noise_floor_dbfs`, `rms_dbfs` or `peak_dbfs`)
   plus `threshold_offset_db`. It also gives a `target_gr_db`, the gain reduction the setting should
   produce. The planner starts at the middle of the offset range and refines closed-loop towards the
   target GR. This also keeps rules valid after a Trim change. Gain staging changes every absolute
   threshold, which is why `workflow.toml` re-derives thresholds after any Trim move.
3. **Conditions on measurable features.** Every condition names a feature from `workflow.toml [features]`.
   Tonal conditions use `band_dev_db.<band>`: band energy minus the TargetProfile's band energy, after
   loudness matching and 1/3-octave smoothing of the target. Measuring against a smoothed tonal-balance
   target rather than a flat line follows the finding that commercial mixes share a characteristic
   long-term spectral contour [pestana2013]. The deviation from that contour is what engineers hear as
   "too much low-mid". The nine named bands (`sub` … `air`) are roughly octave-wide, with the names
   engineers use for each region. Each band maps one-to-one to the vocabulary words (mud = low_mid,
   box = mid, harsh = presence and so on).

### 0.3 Grammar
- A condition is `{ feature, op, value }`. Ops are `< <= > >= between outside`. `when` lists are AND and
  `when_any` lists are OR.
- An action is `{ block, ...params }`. A param is a scalar, a `[lo, hi]` range, or a **symbolic value**
  such as `resonance_hz`, `masking_peak_hz`, `band_center`, `useful_range_hz.hi`, `loudness_match` or
  `from_source`. The planner resolves symbolic values from measurements.
- Units are carried by the param suffix (`_hz`, `_db`, `_ms`, `_pct`, `_s`). Q is RBJ bandwidth Q.
- `confidence` (high/medium/low) records how strongly sources agree. The planner should weight
  low-confidence rules less in its cost function and prefer reference-derived targets over them.
- `feature_basis` is free text. It clarifies how a rule's features are normalised, for example "deviation
  from target". M031 turns it into code and tests.

The grammar has a validity checker in this task's working notes. It checks TOML syntax, that every source
key, preset id, antonym and conflict resolves, that feature/band/block names exist, and that all ranges are
ordered. M001/M010 should port it into the `mix-knowledge` crate as a unit test.

### 0.4 Limits of this knowledge
Mixing rules of thumb are starting points, not truths. Pestana and Reiss report that their best-practice
findings indicate preferred strategies and are not dogma [pestana2014]. The KB is therefore advisory.
The closed loop (apply, settle, re-measure, keep or revert; `workflow.toml [iteration]`) is the final
judge, and a reference mix's measured profile always overrides genre defaults. Frequencies vary with the
instrument, player, mic and room. That is why each problem is gated on a measured condition rather than
applied blindly, and why resonances use the measured `resonance_hz`.

## 1. workflow.toml

### 1.1 Phase order
The order is gain staging → clean-up filter → gate → corrective EQ → dynamics → tonal EQ → balance →
space/FX → (optional) bus/master. This is the common teaching order in the standard mixing texts
[senior2011, owsinski2017, izhaki2018], which give these dependency reasons:

| phase | why it comes here |
|---|---|
| `gain_staging` | Every later threshold, and the useful fader range, depends on input level. Digital consoles have fixed headroom above the alignment level [katz2015, ebu_r68]. |
| `cleanup_filter` | Subsonic rumble, handling noise and low-frequency bleed waste headroom. They also drive compressor detectors and add to low-mid build-up. Removing them first makes later decisions cleaner [senior2011]. |
| `gate_expand` | Gating decides what reaches the EQ and compressor. Bleed left in raises the compressor's view of the noise floor [senior2011, sos_gates]. |
| `corrective_eq` | Resonances and boxy or muddy regions should not drive the compressor's detector. Cutting them before dynamics avoids compressing on unwanted energy [izhaki2018]. |
| `dynamics` | Level control precedes tonal judgements, which change with level (equal-loudness contours [moore2012]). |
| `tonal_eq` | Broad shaping and masking resolution are judged in context once levels are stable [senior2011]. |
| `balance` | Faders and pan. Pan comes before the final fader trims because the pan law changes perceived level. Low frequencies stay centred [pestana2014, owsinski2017]. |
| `space_fx` | Reverb and delay mask detail and depend on the dry balance, so they go last [senior2011, izhaki2018]. |
| `bus_master` | Main-bus processing is `conditional` in the remit (ARCHITECTURE.md) and is a light touch only [katz2015]. |

`gate_tolerance` lets a later phase run while an earlier one is "good enough". For example, tonal work may
start once input peaks are below −1 dBFS, even if gain staging is not perfect. `[iteration]` encodes
re-checking tone after dynamics and balance changes, because both shift spectral and masking measurements.
It also caps moves per strip per pass, which keeps actuation human-like (M040). It waits 1.5–4 s between
moves because short-term loudness integrates over 3 s [itu_bs1770, ebu_r128]. It keeps a move only if the
normalised distance improves by at least 5 %.

### 1.2 Loudness and headroom targets
- **Channel peaks −18..−6 dBFS, RMS around −30..−18 dBFS.** EBU R68 aligns 0 dBu analogue to −18 dBFS, which
  leaves 18 dB above alignment for peaks [ebu_r68]. Katz argues for working with headroom rather than
  near full scale [katz2015]. Running around this region keeps transient peaks clear of clipping. It also
  means input processing sees similar levels across channels. `clip_guard_dbfs = −3` makes Trim the first
  move whenever an input peaks near full scale. WING preamp and trim ranges come from [behringer_wing].
- **Main bus peaks −12..−3 dBFS and true peak ≤ −1 dBTP.** True peak is measured as in ITU-R BS.1770
  [itu_bs1770]. The −1 dBTP ceiling matches the published streaming and broadcast guidance.
- **Delivery profiles.** These apply only to a recording/stream tap of the main mix.
  - EBU R128 broadcast: −23 LUFS ±0.5 LU, max −1 dBTP [ebu_r128].
  - ATSC A/85: −24 LKFS, max −2 dBTP [atsc_a85].
  - AES TD1004: streaming and network playback around −16 LUFS (range −18..−16), max −1 dBTP
    [aes_td1004]. Secondary sources differ on the exact figure in the text, so the KB stores a range.
  - Spotify: normalises to −14 LUFS integrated and recommends true peak at or below −1 dBTP; some
    versions of its help page suggest −2 dBTP for loud masters [spotify_loudness].
  - Apple Music: about −16 LUFS (industry-reported).
  - Live FOH: no LUFS target, because SPL depends on the venue. Only a true-peak ceiling and a moderate PLR
    window (8–16 dB) to protect dynamics.
- **Reference matching.** Mix and reference are both normalised to the same integrated loudness before
  spectral comparison, so a loudness difference does not read as a tonal one. This is the core idea
  behind reference-matching tools such as Matchering; the KB uses the concept only, not its code
  [matchering_concept]. Tolerances of about ±2 dB per band (wider at the spectral extremes), 1 LU for
  level and 0.5 dB/oct for tilt are a little above typical level just-noticeable differences for broadband
  changes (around 1 dB [moore2012]). They are widened because live programme varies from section to
  section. They are starting values for M031 to tune on fixtures.

### 1.3 Features and bands
The feature list is the contract between M020 (analysis), M003 (definitions) and the rule files. Notes:

- `crest_db` and `plr_db` measure density and compression. PLR (true peak minus integrated loudness) is
  the loudness-era version of crest factor [katz2015].
- `lra_lu` follows EBU Tech 3342 (part of the R128 family) [ebu_r128].
- `masking_db` is the simplified, best-practice-based masking measure used by Hafezi and Reiss: one
  source's band energy against another's while both are active. They argue it is a better fit for
  perceived masking in multitracks than full psychoacoustic models [hafezi2015].
- `sibilance_db`, `transient_db`, `resonance_db` and `feedback_risk_db` are operational detectors for
  the problems engineers name most often. M020/M003 own their exact DSP.

## 2. sources.toml

Each source type gives a useful range, an HPF range, characteristic regions, a list of typical problems
(each gated on a measured condition), and presets for gate, compressor, de-esser and FX, plus pan and
level hints. The frequency regions are the consensus of the standard references [senior2011,
owsinski2017, izhaki2018], microphone-technique guides [shure_mics], and the fundamental ranges of the
instruments (bass low E = 41.2 Hz; vocal fundamentals roughly 85–255 Hz for speech and higher for
singing).

### 2.1 Rules that apply across sources
- **HPF on everything except true low-frequency sources.** Filtering below a source's useful range removes
  rumble, mic handling noise and low-frequency bleed. In a busy mix this is the cheapest way to clean the
  low end [senior2011, owsinski2017]. `start_hz` is deliberately conservative. Kick, bass, floor tom and
  playback get only subsonic filters, and the bass HPF must stay below the lowest fundamental.
  Backing vocals and pads get higher filters than the lead or a solo instrument, which leaves the low-mid
  to the important parts [senior2011].
- **LPF on band-limited sources.** Guitar and bass cabinets have little useful output above about
  6 kHz, so a low-pass filter removes fizz, noise and cymbal bleed without changing the tone [senior2011].
- **Cut regions:**
  - mud 200–400 Hz on vocals, guitars, bass and keys
  - box 300–600 Hz on kick and toms, 400–800 Hz on snare and vocals
  - nasal/honk 0.8–1.5 kHz
  - harshness 2–5 kHz
  - sibilance 5–9 kHz

  These are the problem regions repeated across the references [senior2011, owsinski2017, izhaki2018,
  gibson2019]. They are encoded as broad-to-medium Q cuts of 2–6 dB, applied only when the measured band
  deviation exceeds about 2 dB.
- **Resonances use the measured frequency.** Drum ring, cabinet boom and acoustic guitar body resonance
  vary per instrument. The rules use `resonance_hz` with narrow Q, which is the measurement version of
  the "boost and sweep, then cut" technique [senior2011, izhaki2018].
- **Live feedback** (`vox_ring`) uses the narrowest Q and the smallest cut that stops the ring. This
  follows standard live-sound practice [shure_mics, owsinski2017].
- **Level-dependent problems use dynamic processing.** Examples are vocal harshness on belted notes and
  sibilance. A static cut would dull the quiet passages, so these go to a dynamic EQ or de-esser
  [senior2011, izhaki2018].

### 2.2 Per-source notes
- **Kick.** The weight is in the 45–100 Hz fundamental. Beater click at 2–5 kHz gives definition on small
  speakers. The 250–500 Hz "box" is the most common cut. Kick and bass need a low-end split (§4.4).
- **Snare top/bottom.** Body is 150–250 Hz, crack 2–5 kHz, wires 5–10 kHz. The bottom mic faces the top
  mic, so it usually needs its polarity inverted [owsinski2017, shure_mics]. The KB states this as a
  measurable check (the low-band sum of top + bottom should rise). Hi-hat bleed is handled by expansion
  rather than EQ, because an EQ cut would also remove the snare's snap.
- **Toms.** Toms play rarely and pick up cymbals and the rest of the kit. Gating them is standard practice
  and removes low-mid build-up from open tom mics [senior2011]. Release follows the drum's decay.
- **Overheads/room.** Their HPF depends on their role: whole-kit image (80–150 Hz) or cymbals only
  (300–500 Hz). Close mics and overheads need time-alignment and polarity checks; snare arrival in the
  overheads is a few milliseconds late. Room mics are often compressed hard on purpose [senior2011,
  owsinski2017].
- **Bass DI/amp.** Compression to even out note levels is near-universal [senior2011, owsinski2017].
  Midrange "growl" at 0.6–1.2 kHz keeps the bass audible on small systems. DI and amp need alignment.
- **Guitars.** Electric guitars are commonly filtered at both ends and panned off-centre. In dense
  arrangements they are the usual competitors for the vocal's 2–4 kHz region [senior2011].
- **Vocals.** Directional mics add proximity mud. The lead owns the presence band. BVs are filtered
  higher, compressed and de-essed harder, and panned wider so they sit behind the lead [senior2011,
  owsinski2017, izhaki2018].
- **Playback.** Material is already mixed and mastered. Trim it to the channel target, and change only
  level, width or tilt. Watch for negative correlation, which collapses in mono PA zones [katz2015].
- **Pan hints.** Kick, snare, bass and lead vocal go in the centre; this is consistent with the
  best-practice findings that low-frequency and lead elements are centred [pestana2014]. Toms and
  overheads are spread to match the kit image. Guitars, keys and BVs are spread wide.
- **Level hints (`level_rel_mix_lu`).** These are deliberately wide and low-confidence. Balance is the
  most genre-dependent decision, so a reference or per-stem target always overrides them. They exist so
  the system has a sane prior when it is driven only by a description.

## 3. dynamics.toml

### 3.1 Time-constant conventions
Attack and release are given as the time to reach about 63 % of the change. This is the convention set out
in the compressor design tutorial by Giannoulis, Massberg and Reiss, which also explains how detector
choice (peak vs RMS), knee and smoothing topology change the perceived effect [giannoulis2012]. Console
definitions vary. M032 should therefore predict a setting's effect with the adapter's block model
(wing-blocks) rather than assume the numbers match across devices.

### 3.2 Gates and expanders
- **Threshold placement.** The threshold sits a few dB above the noise/bleed floor and at least 10–25 dB
  below the source's peaks (`threshold_clamp`). This follows the standard gate set-up procedure: open on
  every wanted hit, stay shut on bleed [sos_gates, senior2011]. The clamp gap is wider for expanders on
  quiet, nuanced sources.
- **Fast attack and source-specific hold/release.** Drum gates use 0–1 ms attack so transients survive.
  Release roughly follows the instrument's decay (short for kick, longer for toms) [sos_gates].
- **Key filters.** A key LPF on the kick gate and a key HPF on the snare expander stop the wrong drum
  opening the gate [senior2011].
- **Expanders instead of gates** for snare (ghost notes), amps (sustain) and vocals (breaths and soft
  words), with limited range (3–15 dB) so the effect stays inaudible [senior2011].
- **`gate_when_useful`.** Gating needs at least 20 dB between hits and bleed, and gaps between notes
  (activity under 60 %). Otherwise the gate chops the sound or never closes.
- **`gate_chatter`.** If the noise floor sits within 12 dB below the threshold, raise the threshold and
  add hold.

### 3.3 Compressor presets
The presets give ratio, attack, release and target gain reduction, collected from the standard texts
[senior2011, owsinski2017, izhaki2018]:

| preset | ratio | attack ms | release ms | GR dB | rationale |
|---|---|---|---|---|---|
| kick/snare/tom | 3–6 | 5–30 | 50–300 | 3–6 | Attack long enough to pass the transient (punch); release short enough to recover before the next hit |
| overhead | 2–3 | 20–50 | 100–300 | 1–3 | Heavy overhead compression pumps the cymbals |
| room_crush | 8–20 | 1–5 | 50–200 | 8–15 | Deliberate effect, often blended in parallel |
| bass | 3–8 | 5–30 | 80–250 | 3–8 | Evens out notes; release longer than one cycle of the lowest note avoids distortion |
| vocal | 3–4 | 2–15 | 50–200 | 3–8 | Steadies level; above about 8 dB GR, use two stages (fast peak catcher + slow leveller) |
| BV | 4–6 | 3–10 | 50–150 | 4–10 | Steadier, sits behind the lead |
| speech | 2.5–4 | 5–15 | 100–250 | 3–6 | Intelligibility and steady level [shure_mics] |
| drum bus | 3–4 | 10–30 | 100–200 | 2–4 | Glue, often parallel |
| mix_bus_glue | 1.5–2 | 10–30 | 100–400 | 1–3 | Light glue only; main bus is conditional in the remit [senior2011, katz2015] |
| main_peak_limiter | ≥20 | 0–1 | 50–200 | 0–2 | Overload protection that should almost never act [katz2015] |

`makeup = "loudness_match"`, together with the `makeup_after_change` rule, puts into practice the
principle that a dynamics change must be judged at matched loudness. A louder signal is easily mistaken
for a better one [senior2011, katz2015]. In the closed loop this also keeps the balance from drifting when
GR changes.

In live sound, makeup gain on a vocal also reduces the feedback margin. This is noted on `vocal_comp`.

### 3.4 Dynamics rules
- `dyn_too_dynamic` / `dyn_squashed`: move GR towards the target crest and LRA in steps of no more than
  3 dB.
- `dyn_lost_punch`: if transients are weak while a compressor is acting, lengthen the attack before
  reducing GR. A fast attack shaves the transient [senior2011, giannoulis2012].
- `dyn_pumping`: if gain reduction is large and the material is bass-heavy, add a key HPF or lengthen the
  release. Low frequencies drive the detector and cause audible pumping [senior2011, izhaki2018].
- `deess_when_sibilant`: de-ess before any air-shelf boost, because a boost raises sibilance
  [senior2011]. The de-esser targets 5–8.5 kHz, lower for male and higher for female voices, tuned to
  the measured peak in the sibilance band.
- `mix_bus_too_dynamic`: lowest priority, conditional on the remit. Fixing strips comes first.

## 4. eq.toml

### 4.1 Q vocabulary and limits
- **Q classes.** notch 6–10, narrow 3–6, medium 1–3, broad 0.5–1, plus shelves. They map to the usual
  advice: narrow cuts for resonances and feedback, broad gentle boosts for tone [senior2011,
  izhaki2018]. Narrow boosts sound like resonances, so `boost_q_max = 2`.
- **Cut before boost.** `cut_before_boost`, `boost_penalty = 1.6` and the deficit rule (look for an
  excess elsewhere first) encode this widely taught preference. Cuts remove the masking or excess that
  makes a region seem lacking. They also cost no headroom and add less audible phase colour at the
  frequencies that matter [senior2011, owsinski2017, izhaki2018].
- **Size limits.** Boosts are typically 0.5–3 dB and at most 6 dB. Cuts go to 12 dB, and feedback notches
  to 15 dB. Each step is at most 3 dB, and total boost per strip at most 8 dB. A boost beyond about 6 dB
  usually signals a source, mic or arrangement problem that EQ cannot fix [owsinski2017, senior2011]. The
  per-step cap also serves the human-like actuation rules (ARCHITECTURE: EQ gain at most 3 dB/s) and
  closed-loop stability.
- **Order within a strip:** filter → notches → region cuts → dynamics → broad boosts → air shelf. This
  mirrors the phase order and keeps de-essing ahead of top-end boosts.
- **Loudness-matched evaluation** applies to EQ as well: a cut lowers loudness, and the loop must not
  undo it simply because the result is quieter [katz2015].

### 4.2 Mix-level tonal rules
When the whole mix deviates in a band, the fix goes on the strips that contribute most to that band
(`prefer_fix_at_source`), not on the main EQ. Mix engineers fix sources first. Main-bus EQ is
`conditional` in the remit and changes everything at once. Pestana et al. found that commercial mixes
converge on a common long-term spectral target [pestana2013]. That is the basis for using the reference
(or genre) LTAS as the target and its tilt as a separate correctable quantity. A whole-spectrum tilt is the
one case where bus/main EQ (WING bus/main tilt ±6 dB) is the natural tool, if the remit allows it.
`mix_low_end_mono` centres sub-120 Hz contributors. Low frequencies off-centre waste headroom and give
uneven coverage, and the best-practice findings place low-frequency content in the centre [pestana2014,
owsinski2017, katz2015].

### 4.3 Masking resolution
- **Measure.** `masking_db` per band between two strips that are active at the same time. A conflict
  means at least 3 dB of masking with at least 30 % overlap. This is the simplified, best-practice-based
  measure that Hafezi and Reiss showed reduces perceived masking when it drives automatic EQ. Their
  offline system beat amateur mixes and came close to professional ones [hafezi2015].
- **Priority order.** On a conflict, one source keeps the band and the other yields. Lead vocal and speech
  come first, then kick, snare and bass, with everything else after. This is the conventional hierarchy
  for pop and rock mixing [senior2011, izhaki2018]. A description such as "drum-heavy" can reorder it
  through the TargetProfile.
- **Strategies, cheapest first:**
  1. `complementary_cut`: cut the yielding source at the masking peak.
  2. `mirrored_eq`: cut one source and give the other a small, broad boost at the same frequency. This
     is a common practice that Wakefield and Dewey tested [wakefield2015].
  3. `pan_apart`: in their study, listeners preferred tools that panned both tracks, and panning costs no
     tonal change [wakefield2015]. It is excluded for the centred elements and below 200 Hz.
  4. `dynamic_duck`: a side-chained dynamic EQ or compressor, for intermittent masking such as vocal
     phrases over guitars or kick hits over bass [senior2011, izhaki2018].
  5. `level`: the last resort, because it changes the requested balance.
- **Low-end split.** Kick and bass each own one region (by default kick 50–80 Hz, bass 80–120 Hz, swapped
  for sub-heavy synth bass). This is the standard way to stop the two blurring [senior2011, owsinski2017].

## 5. fx.toml

### 5.1 Standard FX rack
The usual FOH layout:
- vocal plate (1.2–2.5 s, 20–80 ms predelay)
- hall for ballads, pads and strings (1.8–3 s)
- short drum room (0.5–1.2 s)
- snare plate (1–1.8 s)
- tempo delay for the vocal (dotted eighth or quarter, 15–35 % feedback, filtered)
- optional slapback (80–160 ms) and doubler

These are the conventional starting points in the references [owsinski2017, izhaki2018, senior2011]. Every
reverb return is high-passed (100–400 Hz), and delay repeats are band-limited. Low-frequency reverb tails
cloud the mix, and darker, filtered repeats sit behind the dry signal [senior2011, izhaki2018]. On WING
these FX buses map to bus-inserted FX slots 1–8 (models.md section 6).

### 5.2 Depth in measurable terms
Send depth is expressed as `wet_rel_dry_lu`: the loudness of the source's reverb or delay contribution
relative to its dry level. A send level in dB is not used because it means nothing without the return
level and the FX gain staging. The adapter converts between the two. The ranges follow the usual
"felt more than heard" advice for upfront vocals (wet about 10–18 LU below dry). BVs are a little wetter
than the lead, which pushes them back. Kick and bass stay essentially dry [senior2011, owsinski2017,
izhaki2018].

Depth perception in general works like this: a source sounds further away when it is quieter, when the
ratio of reverb to direct sound rises, when its high frequencies drop and when its transients soften
[izhaki2018, moore2012]. That is the basis of the `distant`/`present` vocabulary terms.

### 5.3 Tempo
- Delay times derive from the beat: quarter = 60000/BPM ms, plus eighth, dotted eighth and sixteenth.
- Predelay is a small note value clamped to 10–80 ms. Longer predelay separates the dry voice from the
  tail and preserves intelligibility.
- Decay is set so the tail dies within about ½–2 bars depending on tempo.

These are the standard tempo-sync heuristics [owsinski2017]. `fx_tempo_sync` re-runs them when the
detected tempo moves by more than 3 %.

### 5.4 FX rules
- `fx_too_dry` / `fx_too_wet`: move sends towards the reference's reverb proxy, in an order that starts
  with the sources whose depth matters most to the listener.
- `fx_reverb_mud`: raise the return's lo-cut before touching the sources.
- `fx_sibilant_reverb`: de-ess first, then darken the return. Reverb exaggerates sibilants.
- `fx_no_low_end_sends`: keep kick and bass dry.
- `fx_vocal_intelligibility`: add predelay and reduce wet when the vocal's presence band is masked.

These are paraphrased from [senior2011, izhaki2018, owsinski2017].

### 5.5 Live context
A reverberant venue adds its own reverb, so FOH needs less artificial reverb than a studio reference
implies. `[context]` lowers wet levels by 3–6 LU in reverberant rooms. This is low-confidence practice
knowledge; the closed loop measures at the capture point, which may not be the audience position.

## 6. vocabulary.toml

### 6.1 Approach
Words move the target; they never move the console directly. The planner then reaches the shifted
target through the normal loop, so a description cannot bypass the remit, limits or workflow order. Each
term gives:
- `deltas` on features: band deviations, level, crest/PLR/LRA, width/correlation, reverb proxy, transient
  and sibilance
- a polarity: `desirable` ("warm") vs `problem` ("muddy"; the description usually asks to remove it, so
  the deltas are negated)
- a default scope, and optionally a source/family scope ("punchy drums", "warm bass")
- conflicts, which are resolved by summing deltas, clamped by `[limits]`, with the trade-off reported to
  the user (M024/M030)

Intensity modifiers scale the deltas: "slightly" ×0.5, "very" ×1.5 and "extremely" ×2. Negation words
("not", "less", "too") flip the sign.

### 6.2 Evidence for the mappings
- **Brightness** is the most robust timbre descriptor across languages and studies. It tracks spectral
  centroid and high-frequency emphasis (`bright`/`dark`, high confidence) [zacharakis2014, saitis2019].
  Faster attacks also make sounds read as brighter, so `crisp` includes a transient delta.
- **Warmth.** In the semantic-audio datasets collected from engineers using EQ and compressor plug-ins
  (SAFE), "warm" and "bright" are among the most frequent EQ descriptors and form roughly opposite poles
  [stables2014, deman2016]. The KB maps warm to low-mid-low (120–250 Hz) emphasis with reduced upper
  bands. SocialEQ found that "warm" is used consistently within one listener but differently across
  listeners [cartwright2013]. The term is therefore `medium` confidence, and the planner should let a
  reference profile override it.
- **Thickness/fullness, thinness and boominess** follow the low-frequency "fullness/depth" semantic
  dimension found in timbre studies [zacharakis2014] and the standard engineering frequency charts
  [gibson2019, owsinski2017].
- **Muddy, boxy, nasal, harsh and sibilant** are the region names used throughout the engineering
  literature (§2.1). They are encoded as excesses in the corresponding named bands, with problem
  polarity. Harshness sits in 2–5 kHz, where the ear is most sensitive according to the equal-loudness
  contours, so small excesses there stand out [moore2012].
- **Punchy** = transients survive, plus low-end weight without low-mid clutter. This is how the
  references explain punch: compressor attack and transient preservation, kick/bass separation
  [senior2011, izhaki2018, giannoulis2012]. It is `medium` confidence.
- **Dynamic/compressed and loud/quiet** map directly onto crest, PLR, LRA and LUFS [katz2015, ebu_r128].
  `loud` is bounded by the delivery ceilings.
- **Wide/narrow** map to width (side-to-mid) and correlation. They are hard-limited by mono
  compatibility (`low_correlation` ≥ 0.8, `correlation` ≥ 0.2) [izhaki2018, pestana2014].
- **Wet/dry, distant/present** map to the reverb proxy, level, high-frequency content and transient cues
  of depth (§5.2).
- **vocal_forward** shifts the lead vocal's target level up. It prefers carving the competitors' presence
  band over raw level, consistent with the masking strategies (§4.3). Balance terms for drums and
  guitars are direct level-target shifts.
- **vintage/modern** are low-confidence character terms. Pestana et al. observed that the long-term
  spectra of commercial releases changed over the decades [pestana2013]. The deltas are only a coarse
  stand-in, and a reference is strongly preferred.
- **natural/balanced** has no delta. It tightens tolerances instead.

### 6.3 Known gaps
- The mappings are priors. M024 should log which terms users apply and how they rate the result, so the
  deltas can be refitted, as SAFE and SocialEQ did with crowd data [stables2014, cartwright2013].
- Genre-specific balance and tonal templates (EDM vs jazz vs worship) are not encoded. The
  reference/TargetProfile path (M022) covers them.

## 7. Proposed follow-ups (outside M005 scope)
- Port the TOML cross-reference checker into `crates/mix-knowledge` as a unit test (M001/M010 scaffold).
- Fixture-based tuning of the tolerances in `[targets.reference_matching]` and the masking thresholds
  (M031/M012).
- Verify the WING attack/release definitions against wing-blocks so that `attack_ms`/`release_ms` presets
  translate exactly (M032).
- Optional genre tonal-balance curves as a data file (`data/knowledge/genres.toml`) if M022's reference
  path is unavailable.

## Bibliography
Books (cited, not quoted):
- [senior2011] Mike Senior, *Mixing Secrets for the Small Studio*, Focal Press, 2011 (2nd ed. 2018).
- [owsinski2017] Bobby Owsinski, *The Mixing Engineer's Handbook*, 4th ed., 2017.
- [izhaki2018] Roey Izhaki, *Mixing Audio: Concepts, Practices and Tools*, 3rd ed., Routledge, 2018.
- [katz2015] Bob Katz, *Mastering Audio: The Art and the Science*, 3rd ed., Focal Press, 2015.
- [gibson2019] David Gibson, *The Art of Mixing*, 3rd ed., Routledge, 2019.
- [moore2012] Brian C. J. Moore, *An Introduction to the Psychology of Hearing*, 6th ed., 2012.

Papers:
- [giannoulis2012] D. Giannoulis, M. Massberg, J. D. Reiss, "Digital Dynamic Range Compressor Design – A
  Tutorial and Analysis", *JAES* 60(6), 2012.
- [pestana2014] P. D. Pestana, J. D. Reiss, "Intelligent audio production strategies informed by best
  practices", AES 53rd Int. Conf. on Semantic Audio, 2014, pp. 300–308.
  https://aes2.org/publications/elibrary-page/?id=17121
- [pestana2013] P. D. Pestana, Z. Ma, J. D. Reiss, A. Barbosa, D. A. A. Black, "Spectral characteristics of
  popular commercial recordings 1950–2010", AES 135th Convention, paper 8960, 2013.
  https://aes2.org/publications/elibrary-page/?id=17010
- [hafezi2015] S. Hafezi, J. D. Reiss, "Autonomous multitrack equalization based on masking reduction",
  *JAES* 63(5), 312–323, 2015. https://aes2.org/publications/elibrary-page/?id=17637
- [wakefield2015] J. Wakefield, C. Dewey, "An investigation into the efficacy of methods commonly employed
  by mix engineers to reduce frequency masking in the mixing of multitrack musical recordings", AES
  138th Convention, paper 9341, 2015. https://aes2.org/publications/elibrary-page/?id=17765
- [stables2014] R. Stables, S. Enderby, B. De Man, G. Fazekas, J. D. Reiss, "SAFE: A system for the
  extraction and retrieval of semantic audio descriptors", ISMIR 2014 (late-breaking).
  https://www.open-access.bcu.ac.uk/3255/
- [deman2016] R. Stables, B. De Man, S. Enderby, J. D. Reiss, G. Fazekas, T. Wilmering, "Semantic
  description of timbral transformations in music production", ACM Multimedia 2016.
  https://qmro.qmul.ac.uk/xmlui/handle/123456789/22150
- [cartwright2013] M. Cartwright, B. Pardo, "Social-EQ: crowdsourcing an equalization descriptor map",
  ISMIR 2013. https://archives.ismir.net/ismir2013/paper/000173.pdf
- [zacharakis2014] A. Zacharakis, K. Pastiadis, J. D. Reiss, "An interlanguage study of musical timbre
  semantic dimensions and their acoustic correlates", *Music Perception* 31(4), 2014.
- [saitis2019] C. Saitis, S. Weinzierl, "The semantics of timbre", in *Timbre: Acoustics, Perception, and
  Cognition*, Springer Handbook of Auditory Research 69, 2019.

Standards and vendor documents:
- [itu_bs1770] ITU-R BS.1770, loudness and true-peak measurement. https://www.itu.int/rec/R-REC-BS.1770
- [ebu_r128] EBU R 128 (and Tech 3341/3342), loudness normalisation. https://tech.ebu.ch/publications/r128
- [ebu_r68] EBU R 68, alignment level in digital audio equipment. https://tech.ebu.ch/publications/r068
- [aes_td1004] AES TD1004.1.15-10, loudness of audio streaming and network file playback.
- [atsc_a85] ATSC A/85, loudness for digital television.
- [spotify_loudness] Spotify for Artists, "Loudness normalization".
  https://support.spotify.com/artists/article/loudness-normalization/
- [shure_mics] Shure educational publications on microphone techniques (recording and live).
- [sos_gates] Sound On Sound technique articles on gates/expanders and compression set-up.
- [behringer_wing] Behringer WING documentation, via wing-simulator `docs/research/models.md` and
  `signal-flow.md`.
- [matchering_concept] Matchering (GPL), referenced for the concept of loudness-normalised reference
  matching only. https://github.com/sergree/matchering
- [practice] Consensus engineering practice where no single citable source exists. Always low
  confidence.
