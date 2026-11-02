#!/usr/bin/env bash
# build.sh — build NSA5GFix.apk: the notification-only wrapper app whose
# root-side child is nsa5gfix-diagd (nsg-unlock app style).
#
# Gates (fail closed):
#   1. nsa5gfix-diagd.rs recompiles for host AND the selftest passes (the
#      synthetic 0xB823 round-trip is the parser's positive control).
#   2. The APK's packaged daemon is byte-identical to the binary just
#      built from source (staged with a sha256 stamp check).
set -euo pipefail
cd "$(dirname "$0")"
ROOT=$(cd .. && pwd)

step() { printf '\n=== %s ===\n' "$*"; }

step 'Build + selftest nsa5gfix-diagd (host gate)'
rustc -O "$ROOT/nsa5gfix-diagd.rs" -o "$ROOT/nsa5gfix-diagd-host"
"$ROOT/nsa5gfix-diagd-host" selftest

step 'Build nsa5gfix-diagd for Android (static aarch64 musl)'
rustc -O --target aarch64-unknown-linux-musl \
    -C linker=rust-lld -C link-self-contained=yes \
    "$ROOT/nsa5gfix-diagd.rs" -o "$ROOT/nsa5gfix-diagd"
file "$ROOT/nsa5gfix-diagd" | grep -q 'ARM aarch64' || { echo 'not an aarch64 binary' >&2; exit 1; }

step 'Stage daemon into the APK (jniLibs)'
mkdir -p app/src/main/jniLibs/arm64-v8a
cp "$ROOT/nsa5gfix-diagd" app/src/main/jniLibs/arm64-v8a/libnsa5gfix-diagd.so
chmod 644 app/src/main/jniLibs/arm64-v8a/libnsa5gfix-diagd.so
shasum -a 256 "$ROOT/nsa5gfix-diagd" app/src/main/jniLibs/arm64-v8a/libnsa5gfix-diagd.so

step 'Keystore (sideload key, same convention as nsg-unlock)'
if [ ! -f debug.keystore ]; then
    keytool -genkeypair -keystore debug.keystore -alias nsa5gfix \
        -storepass nsa5gfix -keypass nsa5gfix -keyalg RSA -keysize 2048 \
        -validity 10000 -dname 'CN=nsa5gfix, O=nsa5gfix'
fi

step 'Assemble APK'
gradle --console=plain -q :app:assembleRelease
cp app/build/outputs/apk/release/app-release.apk NSA5GFix.apk

step 'Gate: packaged daemon is byte-identical to the fresh build'
TMP=$(mktemp)
unzip -p NSA5GFix.apk lib/arm64-v8a/libnsa5gfix-diagd.so > "$TMP"
PKG_SHA=$(shasum -a 256 "$TMP" | awk '{print $1}')
SRC_SHA=$(shasum -a 256 "$ROOT/nsa5gfix-diagd" | awk '{print $1}')
rm -f "$TMP"
if [ "$PKG_SHA" != "$SRC_SHA" ]; then
    echo "ERROR: APK-packaged daemon differs from the fresh build" >&2
    echo "  packaged: $PKG_SHA" >&2
    echo "  built:    $SRC_SHA" >&2
    exit 1
fi
printf 'packaged daemon sha256 ok: %s\n' "$SRC_SHA"
ls -la NSA5GFix.apk
printf 'done — install with: adb install -r app/NSA5GFix.apk\n'
