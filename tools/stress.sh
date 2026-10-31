#!/usr/bin/env bash
# Flaky-test stress run: N x `cargo test --workspace` under a CPU hog (one `yes` per core) and high test threads.
# Usage: tools/stress.sh [runs=30] [out-dir=target/stress]. Failing runs are kept as <out-dir>/run-N.log; the summary
# lists every failed test. Always kills the hogs on exit. Never touches ports 2222/2223 (tests bind port 0).
set -u
RUNS=${1:-30}
OUT=${2:-target/stress}
NCPU=$(getconf _NPROCESSORS_ONLN 2>/dev/null || nproc)
mkdir -p "$OUT"; rm -f "$OUT"/run-*.log
cargo test --workspace --no-run -q || exit 2
HOGS=()
cleanup() { for p in "${HOGS[@]:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null; done; }
trap cleanup EXIT INT TERM
for _ in $(seq "$NCPU"); do yes > /dev/null & HOGS+=($!); done
fail=0
for i in $(seq "$RUNS"); do
  if RUST_TEST_THREADS=$((NCPU * 4)) cargo test --workspace --no-fail-fast > "$OUT/run-$i.log" 2>&1; then
    rm "$OUT/run-$i.log"; echo "run $i: ok"
  else
    fail=$((fail + 1)); echo "run $i: FAILED ($OUT/run-$i.log)"
  fi
done
echo "== $fail/$RUNS runs failed"
[ "$fail" -gt 0 ] && grep -h "^test .* FAILED$\|panicked at" "$OUT"/run-*.log | sort | uniq -c | sort -rn
[ "$fail" -eq 0 ]
