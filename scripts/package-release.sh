#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEMPLATE_DIR="$ROOT/magisk"
OUT_DIR="${OUT_DIR:-$ROOT/freezeitRelease}"
DAEMON="${DAEMON:-$ROOT/freezeitDaemon/target/aarch64-linux-android/release/freezeit}"
APK="${APK:-}"
EXPECTED_VERSION="${EXPECTED_VERSION:-3.3.1SelfUse}"
EXPECTED_VERSION_CODE="${EXPECTED_VERSION_CODE:-303001}"
SOURCE_REPOSITORY_URL="${SOURCE_REPOSITORY_URL:-https://github.com/szh1118/freezeit_cos16}"
RELEASE_KIND="${RELEASE_KIND:-released}"
ALLOW_DIRTY="${ALLOW_DIRTY:-0}"

fail() { echo "release packaging failed: $*" >&2; exit 1; }
prop() { awk -F= -v key="$1" '$1 == key {print substr($0, index($0, "=") + 1); exit}' "$2"; }
require_file() { [[ -f "$1" ]] || fail "missing file: $1"; }

require_file "$TEMPLATE_DIR/module.prop"
require_file "$ROOT/LICENSE"
[[ "$RELEASE_KIND" == released || "$RELEASE_KIND" == candidate ]] || fail "RELEASE_KIND must be released or candidate"
dirty_status="$(git -C "$ROOT" status --porcelain=v1 --untracked-files=all)"
if [[ -n "$dirty_status" ]]; then
  [[ "$RELEASE_KIND" == candidate && "$ALLOW_DIRTY" == 1 ]] \
    || fail "working tree is dirty; only RELEASE_KIND=candidate ALLOW_DIRTY=1 may package it"
fi
[[ "$EXPECTED_VERSION" =~ ^[0-9A-Za-z][0-9A-Za-z._+-]{0,63}$ ]] || fail "invalid version value"
[[ "$EXPECTED_VERSION_CODE" =~ ^[0-9]{1,10}$ ]] || fail "invalid versionCode value"
[[ "$(prop version "$TEMPLATE_DIR/module.prop")" == "$EXPECTED_VERSION" ]] || fail "module version must be $EXPECTED_VERSION"
[[ "$(prop versionCode "$TEMPLATE_DIR/module.prop")" == "$EXPECTED_VERSION_CODE" ]] || fail "module versionCode must be $EXPECTED_VERSION_CODE"

if [[ -z "$APK" ]]; then
  mapfile -t apks < <(find "$ROOT/freezeitApp/app/build/outputs/apk/release" -maxdepth 1 -type f -name '*.apk' -print 2>/dev/null | sort)
  [[ ${#apks[@]} -eq 1 ]] || fail "expected exactly one release APK; found ${#apks[@]}"
  APK="${apks[0]}"
fi
require_file "$DAEMON"
require_file "$APK"

readelf -h "$DAEMON" | grep -Eq 'Machine:[[:space:]]+AArch64' || fail "daemon is not an AArch64 ELF: $DAEMON"

APK_METADATA="${APK_METADATA:-$(dirname "$APK")/output-metadata.json}"
require_file "$APK_METADATA"
apk_file="$(basename "$APK")"
mapfile -t apk_metadata_values < <(python3 - "$APK_METADATA" "$apk_file" <<'PY'
import json
import sys

path, expected_file = sys.argv[1:]
try:
    with open(path, encoding="utf-8") as stream:
        data = json.load(stream)
except (OSError, json.JSONDecodeError) as error:
    raise SystemExit(f"invalid APK metadata JSON: {error}")
matches = [item for item in data.get("elements", []) if item.get("outputFile") == expected_file]
if len(matches) != 1:
    raise SystemExit(f"expected one metadata element for {expected_file}, found {len(matches)}")
item = matches[0]
print(item.get("versionName", ""))
print(item.get("versionCode", ""))
PY
)
[[ ${#apk_metadata_values[@]} -eq 2 ]] || fail "cannot parse APK metadata"
apk_version="${apk_metadata_values[0]}"
apk_version_code="${apk_metadata_values[1]}"
[[ "$apk_version" == "$EXPECTED_VERSION" ]] || fail "APK version is $apk_version, expected $EXPECTED_VERSION"
[[ "$apk_version_code" == "$EXPECTED_VERSION_CODE" ]] || fail "APK versionCode is $apk_version_code, expected $EXPECTED_VERSION_CODE"

for forbidden in freezeitARM64 freezeitX64 freezeitRustARM64 freezeitRustX64; do
  [[ ! -e "$TEMPLATE_DIR/$forbidden" ]] || fail "template contains forbidden daemon: $forbidden"
done
[[ ! -e "$TEMPLATE_DIR/freezeit" ]] || fail "template must not contain a daemon binary"
if find "$TEMPLATE_DIR" -maxdepth 1 -type f -name '*.apk' -print -quit | grep -q .; then
  fail "template must not contain an APK"
fi

stage_parent="${STAGING_ROOT:-$ROOT/.release-staging}"
mkdir -p "$stage_parent" "$OUT_DIR"
stage="$(mktemp -d "$stage_parent/package.XXXXXX")"
trap 'rm -rf "$stage"' EXIT
cp -a "$TEMPLATE_DIR/." "$stage/"
cp "$DAEMON" "$stage/freezeit"
cp "$APK" "$stage/freezeit.apk"
cp "$ROOT/LICENSE" "$stage/LICENSE"
chmod 0755 "$stage/freezeit" "$stage/customize.sh" "$stage/service.sh" "$stage/uninstall.sh"

commit="$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || printf unknown)"
dirty=false
if [[ -n "$dirty_status" ]]; then
  dirty=true
  git -C "$ROOT" diff --binary HEAD >"$stage/source.patch"
  printf '%s\n' "$dirty_status" >"$stage/source-state.txt"
  mapfile -t snapshot_files < <(
    git -C "$ROOT" ls-files -co --exclude-standard \
      | while IFS= read -r path; do
          [[ -e "$ROOT/$path" || -L "$ROOT/$path" ]] && printf '%s\n' "$path"
        done \
      | LC_ALL=C sort
  )
  [[ ${#snapshot_files[@]} -gt 0 ]] || fail "dirty candidate source snapshot is empty"
  tar -C "$ROOT" -czf "$stage/source-snapshot.tar.gz" "${snapshot_files[@]}"
fi
daemon_sha="$(sha256sum "$stage/freezeit" | awk '{print $1}')"
apk_sha="$(sha256sum "$stage/freezeit.apk" | awk '{print $1}')"
patch_sha=none
snapshot_sha=none
state_sha=none
if [[ "$dirty" == true ]]; then
  patch_sha="$(sha256sum "$stage/source.patch" | awk '{print $1}')"
  snapshot_sha="$(sha256sum "$stage/source-snapshot.tar.gz" | awk '{print $1}')"
  state_sha="$(sha256sum "$stage/source-state.txt" | awk '{print $1}')"
fi
cat >"$stage/provenance.txt" <<PROVENANCE
format=freezeit-release-provenance-v1
version=$EXPECTED_VERSION
versionCode=$EXPECTED_VERSION_CODE
gitCommit=$commit
releaseKind=$RELEASE_KIND
dirty=$dirty
daemonSource=freezeitDaemon
managerSource=freezeitApp
daemonTarget=aarch64-linux-android
daemonSha256=$daemon_sha
apkSha256=$apk_sha
sourcePatchSha256=$patch_sha
sourceSnapshotSha256=$snapshot_sha
sourceStateSha256=$state_sha
PROVENANCE
cat >"$stage/SOURCE_OFFER" <<SOURCE_OFFER
Freezeit is distributed under GPL-3.0-or-later.
Corresponding source for this package is available at:
$SOURCE_REPOSITORY_URL/tree/$commit

Source commit: $commit
Daemon source: freezeitDaemon/
Android manager source: freezeitApp/
SOURCE_OFFER
(
  cd "$stage"
  find . -mindepth 1 -maxdepth 1 -type f ! -name SHA256SUMS -printf '%P\0' \
    | LC_ALL=C sort -z | xargs -0 sha256sum > SHA256SUMS
)

zip_name="freezeit_oneplus13_android16_selfuse_v${EXPECTED_VERSION}_${EXPECTED_VERSION_CODE}.zip"
out_zip="$OUT_DIR/$zip_name"
rm -f "$out_zip"
(
  cd "$stage"
  mapfile -t entries < <(find . -mindepth 1 -maxdepth 1 -printf '%P\n' | LC_ALL=C sort)
  if command -v zip >/dev/null 2>&1; then
    zip -q -r "$out_zip" "${entries[@]}"
  elif command -v bsdtar >/dev/null 2>&1; then
    bsdtar --format zip -cf "$out_zip" "${entries[@]}"
  else
    fail "zip or bsdtar is required"
  fi
)
"$ROOT/scripts/validate-release-zip.sh" "$out_zip" "$EXPECTED_VERSION" "$EXPECTED_VERSION_CODE"
echo "packaged release: $out_zip"
