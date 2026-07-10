#!/usr/bin/env sh
set -eu

SERIAL="${1:-}"
OUT="${2:-magisk/rom_baseline.prop}"
ADB="${ADB:-adb}"

if [ -n "$SERIAL" ]; then
  ADB="$ADB -s $SERIAL"
fi

getprop_value() {
  # shellcheck disable=SC2086
  $ADB shell getprop "$1" | tr -d '\r'
}

mkdir -p "$(dirname "$OUT")"
{
  echo "rom.android.version=$(getprop_value ro.build.version.release)"
  echo "rom.product=$(getprop_value ro.product.model)"
  echo "rom.device=$(getprop_value ro.product.device)"
  echo "rom.build.id=$(getprop_value ro.build.id)"
  echo "rom.build.incremental=$(getprop_value ro.build.version.incremental)"
  echo "rom.build.fingerprint=$(getprop_value ro.build.fingerprint)"
  echo "rom.security_patch=$(getprop_value ro.build.version.security_patch)"
  # shellcheck disable=SC2086
  echo "rom.kernel=$($ADB shell uname -r | tr -d '\r')"
} >"$OUT"

echo "captured ROM baseline: $OUT"
