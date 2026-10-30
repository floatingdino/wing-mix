#!/usr/bin/env bash
# M052 end-to-end run (docs/milestones/e2e.md): wing-mixd against (a) the embedded sim and (b) a headless wing-sim on
# private ports over the native protocol, fed the local real multitrack. Writes target/e2e/report.md.
#
#   tools/e2e/run.sh                 # everything (~45 min: real time on the wing-sim path)
#   tools/e2e/run.sh smoke           # the fast make-check variant only
#   tools/e2e/run.sh sim             # (a) only (~2 min)
#   tools/e2e/run.sh wing            # (b) only
#
# Env: E2E_SONGS (default: all three matched songs), E2E_MODES (reference,description), E2E_BATCHES (12),
# E2E_MAX_WAIT_S (900 per run), WING_SIM_BIN (default ../wing-simulator/target/release/wing-sim),
# WINGMIX_FIXTURES (default /Users/sam/code/wing-mix/fixtures), WINGMIX_PUBLIC_SNAP, E2E_REPORT.
# Never touches ports 2222/2223, the default shm name, or the user's WingSimulator / WingMix state dirs.
set -euo pipefail
cd "$(dirname "$0")/../.."
what="${1:-all}"
export E2E_SONGS="${E2E_SONGS:-i-know-that-i-know,holy-forever,show-me-your-face}"
export E2E_BATCHES="${E2E_BATCHES:-12}"
report="${E2E_REPORT:-$PWD/target/e2e/report.md}"
export E2E_REPORT="$report"
mkdir -p "$(dirname "$report")"
{
  echo "# wing-mix e2e report"
  echo
  echo "- date: $(date -u +%Y-%m-%dT%H:%M:%SZ), commit $(git rev-parse --short HEAD) ($(git log -1 --format=%s | cut -c1-60))"
  echo "- wing-sim: ${WING_SIM_BIN:-../wing-simulator/target/release/wing-sim}; songs: $E2E_SONGS; batches: $E2E_BATCHES"
} > "$report"
t() { cargo test -p wing-mixd --release --test e2e "$1" -- --ignored --nocapture --test-threads=1; }
case "$what" in
  smoke) cargo test -p wing-mixd --release --test e2e e2e_wing_smoke -- --nocapture ;;
  sim) t e2e_sim_synthetic ;;
  wing) t e2e_wing_real ;;
  all) cargo test -p wing-mixd --release --test e2e e2e_wing_smoke -- --nocapture
       t e2e_sim_synthetic
       t e2e_wing_real ;;
  *) echo "usage: $0 [all|smoke|sim|wing]" >&2; exit 2 ;;
esac
echo "report: $report"
