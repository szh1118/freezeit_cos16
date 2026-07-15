#!/usr/bin/env sh
set -eu

SERIAL="${1:-}"
ROOT="$(CDPATH='' cd "$(dirname "$0")/.." && pwd)"
OUT="${2:-$ROOT/magisk/rom_baseline.prop}"
ADB="${ADB:-adb}"

run_adb() {
  if [ -n "$SERIAL" ]; then
    "$ADB" -s "$SERIAL" "$@"
  else
    "$ADB" "$@"
  fi
}

getprop_value() {
  value="$(run_adb shell getprop "$1")" || return 1
  printf '%s\n' "$value" | tr -d '\r'
}

android_version="$(getprop_value ro.build.version.release)" || exit 1
product="$(getprop_value ro.product.model)" || exit 1
device="$(getprop_value ro.product.device)" || exit 1
build_id="$(getprop_value ro.build.id)" || exit 1
incremental="$(getprop_value ro.build.version.incremental)" || exit 1
fingerprint="$(getprop_value ro.build.fingerprint)" || exit 1
security_patch="$(getprop_value ro.build.version.security_patch)" || exit 1
kernel="$(run_adb shell uname -r)" || exit 1
kernel="$(printf '%s\n' "$kernel" | tr -d '\r')"

mkdir -p "$(dirname "$OUT")"
{
  echo "rom.android.version=$android_version"
  echo "rom.product=$product"
  echo "rom.device=$device"
  echo "rom.build.id=$build_id"
  echo "rom.build.incremental=$incremental"
  echo "rom.build.fingerprint=$fingerprint"
  echo "rom.security_patch=$security_patch"
  echo "rom.kernel=$kernel"
} >"$OUT"

echo "captured ROM baseline: $OUT"
