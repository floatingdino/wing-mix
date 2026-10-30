# Using Wing Mix with your running wing-sim

Wing Mix can work with the Wing Simulator you already run on this Mac (WingSimMenu, `wing-sim`). It then reads
your real scene, and it hears the console's processed audio. You do **not** need a loopback driver. The simulator's
own audio device ("Wing Simulator", 48 in / 48 out) already carries the console's USB audio in both directions.

Facts below are from wing-simulator `docs/research/signal-flow.md` (section 6, USB and output patch) and
`docs/user/install-checklist.md`.

## Which direction is which

| Wing Simulator device | is the console's | carries | who uses it |
|---|---|---|---|
| **input** channels 1-48 (what an app records) | **USB outputs** 1-48 (`Routing > USB outputs`, `io/out/USB/n`) | whatever you patch there: Main L/R, monitor/solo, matrices, raw inputs, user signals | **Wing Mix listens here** |
| **output** channels 1-48 (what an app plays) | **USB inputs** 1-48 (the `USB` source group) | what your DAW plays, e.g. the multitrack you mix | REAPER |

Two consequences:
- Wing Mix only hears what the console patches to its USB **outputs**. A channel number in Wing Mix's Listen
  screen is a USB-out number.
- In REAPER, a track that holds an already-mixed signal must not play to a USB channel that a console channel uses as
  its source. This includes the recorded FOH mix (for example the FOH mix tracks 52/53 in your project). If it does,
  that mix is mixed in a second time. Turn those tracks' hardware outputs off, or point them at an unused channel.

## 1. Patch Main and the solo bus to USB outputs (WING-Edit)

Pick **four USB outputs you are not using** and patch them in WING-Edit (Routing > USB outputs):

| USB out | source (WING-Edit) | what it is |
|---|---|---|
| *a* | MAIN, 1 | Main 1 L |
| *a*+1 | MAIN, 2 | Main 1 R |
| *b* | MON, 1 | Monitor A L: the solo bus while something is soloed (your monitor mix otherwise) |
| *b*+1 | MON, 2 | Monitor A R |

Mix outputs are mono legs: `MAIN 1/2` = main 1 L/R, `MAIN 3/4` = main 2, `MON 1/2` = monitor A (phones),
`MON 3/4` = monitor B (speakers). Solo goes to the monitor(s) set in `cfg/solo/mon` (the console's solo settings).

- Patch Main and the monitor directly, **not through a matrix**. A matrix has its own fader, EQ and delay, so Wing Mix
  would compare the target with something other than the main mix.
- Check what you are overwriting. A recording scene (`wing-record-setup`) uses all 48 USB outputs for the
  recording split, and a show file may already use the top outputs for matrices. If no outputs are free, give up
  four you do not record.
- The host speakers are a separate path: WingSimMenu's Host Output Routing (for example `mon/1 -> "Scarlett" 1/2`)
  plays monitor 1 on a Mac device. It does not change the USB outputs.

Optional, for per-channel diagnosis: patch up to 24 **user signals** (`io/in/USR/n`: source CH n, tap POST =
post-processing, L+R) to further USB outputs. Each one becomes an "input N" tap in Wing Mix.

## 2. Connect Wing Mix to the running wing-sim (Listen screen)

1. **Console**: click *Find consoles*, choose **wing-sim on this Mac (WINGSIM)**, then *Connect*. Wing Mix finds
   it through wing-sim's `status.json`. It connects over the native protocol to `127.0.0.1:2222`, the same way
   WING-Edit does. The line below the button shows the console and the scene. The Remit screen shows your own
   buses from then on.
2. **Audio input**: Source = *Audio device*, Device = **Wing Simulator** (48 ch).
3. **Taps**: Main LR = channel *a* (stereo pair, *a*/*a*+1). Add a tap Solo bus = channel *b* (stereo pair). Add
   *Channel strip* taps for any user signals you patched.
4. Click *Use this input*. Under each tap, Wing Mix shows what the console currently patches to those USB outputs,
   for example `USB 45 ← Matrix 2 L`. It warns when a Main tap is not MAIN 1/2 or a Solo tap is not MON 1/2. Use
   *Check console USB patch* after you change the patch.
5. On first use macOS asks for **Microphone** permission for Wing Mix (audio input). Allow it. Otherwise the device
   delivers silence.

## Embedded simulator instead (no audio setup)

The Listen screen's *Embedded simulator* runs a private wing-sim inside the daemon. It renders a multitrack file
(or the built-in synthetic song), and its taps are fixed. You do not need a device or patching, and nothing on
your real wing-sim changes. To see your own buses there, choose Scene = **Scene file (.snap)…** and pick a copy
of your scene. Wing Mix only reads the file.

- Your running wing-sim's scene is `~/Library/Application Support/WingSimulator/state.snap`.
- WING-Edit / WingSimMenu "Save As…" writes a scene file too.

Role confirmations are stored per console and scene. The embedded sim uses the file name as the scene, for example
`sim|MyShow`.

## Tools (stems, Claude)

The daemon starts `uv` (stem separation) and `claude` (optional free-text fallback). When Wing Mix launches it,
the daemon adds your login shell's PATH and the usual install folders (`~/.local/bin`, `/opt/homebrew/bin`,
`/usr/local/bin`, nvm's node versions). The Target screen shows a notice when a tool is still missing.
`status.json` → `tools` shows where each tool was found.
