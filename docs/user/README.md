# Wing Mix app screenshots

Rendered from the real app (`apps/WingMix`) against `wing-mixd` with the embedded wing-sim (synthetic 16-track song,
degraded scene, reference = the same song through the reference scene, suggest mode). Regenerate:

```sh
cargo build --release -p wing-mixd --example make_fixture && target/release/examples/make_fixture /tmp/wmx-fix 8
make app
WINGMIX_STATE_DIR=/tmp/wmx-app WINGMIX_DEMO=/tmp/wmx-fix WINGMIX_SHOTS=docs/user \
  apps/WingMix/build/WingMix.app/Contents/MacOS/WingMix
```

`screencapture` produced black/failed captures in the headless agent session (no screen-recording permission), so
the app renders each screen itself with SwiftUI `ImageRenderer` (`WINGMIX_SHOTS`); AppKit-backed controls are drawn as
look-alikes in that mode (`Controls.swift`). Files: `target.png`, `listen.png`, `remit.png`, `live.png`.

## Autopilot states (M055)

`live-autopilot.png`: a real autopilot run (moves kept, move log with per-row Undo on the newest kept move, strip
names from the console). `live-stopped.png`: a real STOP. `live-confirmation.png` and `live-safety.png` take the
real snapshot and add a pending confirmation / a feedback trip with an RTA feed (fixtures: the sim cannot produce
them on demand). Same command as above; the run takes about 3 minutes (`Screenshots.swift`).

## Remit bulk editing (M065)

`remit-bulk.png`: the embedded sim loaded with wing-simulator's `fixtures/PUBLIC.snap` ("Scene file (.snap)…"), the
buses whose name contains "IEM" selected, *Set role… monitor* awaiting confirmation (the change list; nothing is
sent before *Apply*). `remit-bulk-applied.png`: after *Apply*, with the one-step *Undo*. Regenerate (about 10 s):

```sh
make app
WINGMIX_STATE_DIR=/tmp/wmx-shot WINGMIX_REMIT_SHOT=/Users/sam/code/wing-simulator/fixtures/PUBLIC.snap \
  WINGMIX_SHOTS=docs/user apps/WingMix/build/WingMix.app/Contents/MacOS/WingMix
```

Using your running wing-sim (console selection, USB-out taps, which direction is which): `setup-wing-simulator.md`.
