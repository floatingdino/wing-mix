#!/bin/sh
# Build WingMix.app into apps/WingMix/build/ (M051).
#
#   apps/WingMix/build.sh            # release build
#   CONFIG=debug apps/WingMix/build.sh
#
# target/release/wing-mixd is copied into Contents/Resources/bin when it exists (`make app` builds it first); the
# app falls back to <repo>/target/release (Info.plist WingMixRepoRoot) and $WINGMIX_BIN_DIR.
# WINGMIX_REPO_ROOT="" keeps a distributed app from pointing at the build checkout.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
CONFIG=${CONFIG:-release}
VERSION=${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n 1)}

cd "$HERE"
swift build -c "$CONFIG"
BIN=$(swift build -c "$CONFIG" --show-bin-path)

APP="$HERE/build/WingMix.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/bin"
cp "$BIN/WingMix" "$APP/Contents/MacOS/WingMix"
sed -e "s|@REPO_ROOT@|${WINGMIX_REPO_ROOT-$ROOT}|" -e "s|@VERSION@|${VERSION:-0.1.0}|" "$HERE/Info.plist.in" \
    > "$APP/Contents/Info.plist"
if [ -x "$ROOT/target/release/wing-mixd" ]; then
    cp "$ROOT/target/release/wing-mixd" "$APP/Contents/Resources/bin/wing-mixd"
else
    echo "build.sh: note: $ROOT/target/release/wing-mixd not built; the app will look in target/release at run time" >&2
fi
codesign --force --deep --sign - "$APP" >/dev/null 2>&1 || echo "build.sh: note: ad-hoc codesign failed" >&2
echo "built $APP"
