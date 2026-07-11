#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EXPECTED_VERSION="${EXPECTED_VERSION:-3.3.1SelfUse}"
EXPECTED_VERSION_CODE="${EXPECTED_VERSION_CODE:-303001}"

"$ROOT/freezeitDaemon/scripts/build-android.sh"
(
  cd "$ROOT/freezeitApp"
  bash ./gradlew :app:assembleRelease --no-daemon
)

mapfile -t apks < <(find "$ROOT/freezeitApp/app/build/outputs/apk/release" -maxdepth 1 -type f -name '*.apk' -print | sort)
[[ ${#apks[@]} -eq 1 ]] || { echo "expected exactly one release APK; found ${#apks[@]}" >&2; exit 1; }

DAEMON="$ROOT/freezeitDaemon/target/aarch64-linux-android/release/freezeit" \
APK="${apks[0]}" \
EXPECTED_VERSION="$EXPECTED_VERSION" \
EXPECTED_VERSION_CODE="$EXPECTED_VERSION_CODE" \
  "$ROOT/scripts/package-release.sh"
