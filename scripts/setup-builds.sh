#!/usr/bin/env bash
set -euo pipefail

# Usage: ./setup-builds.sh [buildroot]
# Default buildroot: build

BUILD_ROOT="${1:-build}"

if ! command -v meson >/dev/null 2>&1; then
  echo "meson not found; please install meson and retry" >&2
  exit 1
fi

setup_dir() {
  local dir="$1"
  if [ -d "$dir" ] && [ -f "$dir/meson-info/meson-info.json" ] 2>/dev/null; then
    echo "Reconfiguring existing Meson build dir: $dir"
    meson configure "$dir" || meson setup --reconfigure "$dir"
  else
    echo "Creating Meson build dir: $dir"
    meson setup "$dir" -Dbuildtype="$2"
  fi
}

setup_dir "$BUILD_ROOT" release
setup_dir "$BUILD_ROOT/debug" debug
setup_dir "$BUILD_ROOT/release" release

echo "Created/ensured build directories: $BUILD_ROOT, $BUILD_ROOT/debug, $BUILD_ROOT/release"
echo "Run './scripts/generate-compdb.sh <builddir>' to generate compile_commands.json and update the repo symlink."
