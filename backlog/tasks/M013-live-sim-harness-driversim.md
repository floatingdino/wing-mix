---
id: M013
title: Live wing-sim harness with DriverSim (real meters, no HAL driver)
stage: S1
status: todo
owner: -
depends: [M011, M012]
branch: -
model: sonnet
---

## Goal
Test harness that runs a headless wing-sim daemon on private ports with `wing_shm::sim::DriverSim` + `--shm-name`, feeding fixture multitrack audio in and capturing outputs, so mix-wing (native protocol, real meters, solo) and the daemon can be integration-tested live without the HAL driver or the user's running wing-sim (which uses 2222/2223). See docs/research/wing-control.md.

## Acceptance criteria
- [ ] Test helper starts/stops the daemon + DriverSim, never touching default ports or the user's state dir
- [ ] Meters from real audio received by mix-wing in a test (#[ignore] if slow)
- [ ] Documented in docs/testing.md

## Log
- 2026-10-30 coordinator: created (follow-up proposed by M002).
