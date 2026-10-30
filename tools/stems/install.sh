#!/usr/bin/env bash
# Installs the optional Wing Mix stem-separation sidecar into <state-dir>/stems (uv venv + model weights).
#
# THIS DOWNLOADS ~1 GB (torch + deps) PLUS MODEL WEIGHTS (htdemucs 84 MB, htdemucs_ft 336 MB, umxhq 143 MB).
# It refuses to run without --approved (HUMAN-ACTIONS #3). Nothing is downloaded at runtime otherwise.
#
# usage: install.sh --approved [--state-dir DIR] [--model htdemucs|htdemucs_ft|htdemucs_6s|umxhq] [--dry-run]
#        install.sh --check [--state-dir DIR]      (offline: report whether a usable install exists)
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
state_dir="${WINGMIX_STATE_DIR:-}"
model="htdemucs"
approved=0 dry=0 check=0

while [ $# -gt 0 ]; do
  case "$1" in
    --approved) approved=1 ;;
    --dry-run) dry=1 ;;
    --check) check=1 ;;
    --state-dir) state_dir="$2"; shift ;;
    --model) model="$2"; shift ;;
    -h|--help) sed -n '2,10p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

if [ -z "$state_dir" ]; then
  if [ "$(uname -s)" = Darwin ]; then state_dir="$HOME/Library/Application Support/WingMix"
  else state_dir="${XDG_STATE_HOME:-$HOME/.local/state}/wingmix"; fi
fi
proj="$state_dir/stems"
marker="$proj/installed.json"

case "$model" in
  htdemucs|htdemucs_ft|htdemucs_6s|umxhq) ;;
  *) echo "unsupported model: $model" >&2; exit 2 ;;
esac

if [ "$check" = 1 ]; then
  if [ -f "$marker" ] && command -v uv >/dev/null 2>&1; then cat "$marker"; exit 0; fi
  echo "stems sidecar not installed (state dir: $state_dir)" >&2; exit 1
fi

if [ "$approved" != 1 ]; then
  echo "refusing to run: this downloads ~1 GB of Python packages and model weights." >&2
  echo "Re-run with --approved once you have approved it (docs/HUMAN-ACTIONS.md #3)." >&2
  exit 3
fi

extra=()
[ "$model" = umxhq ] && extra=(--extra umx)

run() { echo "+ $*" >&2; [ "$dry" = 1 ] || "$@"; }

command -v uv >/dev/null 2>&1 || { echo "uv not found; install it first (https://docs.astral.sh/uv/)" >&2; exit 4; }

echo "state dir: $state_dir; model: $model" >&2
run mkdir -p "$proj/cache"
# Copy the project (pyproject + package) next to the venv; the venv lives in $proj/.venv.
run cp "$here/pyproject.toml" "$proj/pyproject.toml"
run rm -rf "$proj/wingmix_stems"
run cp -R "$here/wingmix_stems" "$proj/wingmix_stems"
run find "$proj/wingmix_stems" -name __pycache__ -type d -prune -exec rm -rf {} +

export TORCH_HOME="$proj/cache/torch" XDG_CACHE_HOME="$proj/cache" UV_PYTHON_PREFERENCE=managed
run uv python install 3.12
run uv sync --project "$proj" --python 3.12 ${extra[@]+"${extra[@]}"}
# One-shot weight fetch (the only other network access).
run uv run --project "$proj" python -m wingmix_stems --fetch-weights "$model"

if [ "$dry" != 1 ]; then
  printf '{"model":"%s","installed_at":"%s"}\n' "$model" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$marker"
fi
echo "done: $proj" >&2
