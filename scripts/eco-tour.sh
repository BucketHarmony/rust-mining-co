#!/usr/bin/env bash
# Record the ecology video tour and encode it.
#   eco-tour.sh [segment,...]   e.g. eco-tour.sh water,meadow (re-records only those)
# Renders on the real GPU when one is present (headless, no window); set
# WGPU_ADAPTER_NAME yourself to choose another adapter. Output:
# target/video/eco_tour.mp4.
set -eu
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
if [ -z "${WGPU_ADAPTER_NAME:-}" ] && command -v nvidia-smi >/dev/null 2>&1; then
  case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*|Windows*) export WGPU_BACKEND="${WGPU_BACKEND:-dx12}" ;;
    *) export WGPU_BACKEND="${WGPU_BACKEND:-vulkan}" ;;
  esac
  export WGPU_ADAPTER_NAME=NVIDIA
fi
export TOUR_SIZE="${TOUR_SIZE:-1280x720}"
[ -n "${1:-}" ] && export TOUR_ONLY="$1"
scripts/e2e.sh eco_tour
python scripts/eco-tour.py
