---
id: M005
title: Research: mixing knowledge base as structured data
stage: S0
status: todo
owner: -
depends: []
branch: -
model: opus
---

## Goal
Encode practical mix-engineering knowledge as data the planner can use: data/knowledge/*.toml + docs/research/mix-knowledge.md. Cover: workflow order (gain staging → HPF → corrective EQ → dynamics → tonal EQ → balance → space/FX), per-source-type defaults and typical problem frequencies (kick, snare, toms, OH, bass DI/amp, guitars, keys, lead/BV vocals, playback, room), compression/gate starting points (ratio, attack, release, threshold relative to signal stats), EQ move vocabulary (cut-before-boost, Q choices, max sensible boost), masking resolution strategies, FX send depth conventions (reverb/delay per source), loudness/headroom targets, and description vocabulary (warm, bright, punchy, airy, muddy, boxy, harsh, wide, vocal-forward...) mapped to measurable target deltas. Cite sources (textbooks, reputable articles) in the doc; no copyrighted text reproduced.

## Acceptance criteria
- [ ] data/knowledge/{workflow,sources,dynamics,eq,fx,vocabulary}.toml with a documented schema
- [ ] docs/research/mix-knowledge.md explaining every rule's rationale and source

## Log
- 2026-10-30 coordinator: created.
