---
id: M005
title: Research: mixing knowledge base as structured data
stage: S0
status: in_progress
owner: agent-M005
depends: []
branch: task/M005
model: opus
---

## Goal
Encode practical mix-engineering knowledge as data the planner can use: data/knowledge/*.toml + docs/research/mix-knowledge.md. Cover: workflow order (gain staging → HPF → corrective EQ → dynamics → tonal EQ → balance → space/FX), per-source-type defaults and typical problem frequencies (kick, snare, toms, OH, bass DI/amp, guitars, keys, lead/BV vocals, playback, room), compression/gate starting points (ratio, attack, release, threshold relative to signal stats), EQ move vocabulary (cut-before-boost, Q choices, max sensible boost), masking resolution strategies, FX send depth conventions (reverb/delay per source), loudness/headroom targets, and description vocabulary (warm, bright, punchy, airy, muddy, boxy, harsh, wide, vocal-forward...) mapped to measurable target deltas. Cite sources (textbooks, reputable articles) in the doc; no copyrighted text reproduced.

## Acceptance criteria
- [ ] data/knowledge/{workflow,sources,dynamics,eq,fx,vocabulary}.toml with a documented schema
- [ ] docs/research/mix-knowledge.md explaining every rule's rationale and source

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 agent-M005: started in worktree .worktrees/M005. Read ARCHITECTURE + wing-simulator models.md/signal-flow.md (WING capabilities: ch EQ = low/high (PEQ|SHV) + 4 PEQ, +/-15 dB, Q 0.44..10; LC 6-24 dB/oct 20..2000 Hz; gate thr -80..0, range 3..60 dB; COMP thr -60..0, ratio 1.1..100, att 0..120 ms, rel 4..4000 ms; FX = 16 stereo slots inserted on buses -> send FX). Next: web research, then TOML files.
- 2026-10-30 agent-M005: wrote data/knowledge/{workflow,sources,dynamics,eq,fx,vocabulary}.toml (each with [schema]; workflow.toml holds shared features/bands/blocks/condition+action grammar/source registry). 19 source types, 5 gate + 15 comp + 2 de-ess presets, 7 FX buses + 7 send presets, 43 vocabulary terms. Cross-reference check (sources keys, presets, antonyms, feature/band/block names, range ordering) passes with a scratch script. Next: docs/research/mix-knowledge.md.
