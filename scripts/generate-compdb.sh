#!/usr/bin/env bash
set -euo pipefail

# Usage: ./generate-compdb.sh [builddir]
# Default builddir: build

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd -P)"
BUILD_DIR="${1:-build}"

if [ ! -d "$BUILD_DIR" ]; then
  echo "Build directory '$BUILD_DIR' not found. Create it with: ./scripts/setup-builds.sh $BUILD_DIR" >&2
  exit 1
fi

if ! command -v ninja >/dev/null 2>&1; then
  echo "ninja not found; please install ninja and retry" >&2
  exit 1
fi

echo "Generating compile_commands.json in $BUILD_DIR..."
# Prefer the generic compdb output (works with Meson-generated targets). If that produces no
# content (some ninja versions/targets may filter), fall back to the cxx/cc-specific query.

ninja -C "$BUILD_DIR" -t compdb > "$BUILD_DIR/compile_commands.json"
# If the file is empty (just []), try the cxx/cc variant as a fallback.
if [ ! -s "$BUILD_DIR/compile_commands.json" ] || grep -qE '^\s*\[\s*\]' "$BUILD_DIR/compile_commands.json"; then
  ninja -C "$BUILD_DIR" -t compdb cxx cc > "$BUILD_DIR/compile_commands.json" 2>/dev/null || true
fi

# Create (or update) symlink at repository root
ln -sf "$PWD/$BUILD_DIR/compile_commands.json" "$REPO_ROOT/compile_commands.json"

echo "Wrote $BUILD_DIR/compile_commands.json and symlinked to $REPO_ROOT/compile_commands.json"
