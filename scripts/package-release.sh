#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEMPLATE_DIR="$ROOT/magisk"
OUT_DIR="${OUT_DIR:-$ROOT/freezeitRelease}"
DAEMON="${DAEMON:-$ROOT/freezeitDaemon/target/aarch64-linux-android/release/freezeit}"
APK="${APK:-}"
EXPECTED_VERSION="${EXPECTED_VERSION:-3.3.2SelfUse}"
EXPECTED_VERSION_CODE="${EXPECTED_VERSION_CODE:-303002}"
SOURCE_REPOSITORY_URL="${SOURCE_REPOSITORY_URL:-https://github.com/szh1118/freezeit_cos16}"
RELEASE_KIND="${RELEASE_KIND:-released}"
ALLOW_DIRTY="${ALLOW_DIRTY:-0}"
BUILD_SESSION_ROOT="${BUILD_SESSION_ROOT:-$ROOT/.release-staging}"
BUILD_SESSION_FILE="${FREEZEIT_BUILD_SESSION_FILE:-}"
BUILD_SESSION_ID="${FREEZEIT_BUILD_SESSION_ID:-}"
BUILD_SESSION_SHA256="${FREEZEIT_BUILD_SESSION_SHA256:-}"
EXPECTED_APK_SIGNER_SHA256="${FREEZEIT_EXPECTED_APK_SIGNER_SHA256:-}"

fail() { echo "release packaging failed: $*" >&2; exit 1; }
prop() { awk -F= -v key="$1" '$1 == key {print substr($0, index($0, "=") + 1); exit}' "$2"; }
require_file() { [[ -f "$1" ]] || fail "missing file: $1"; }
require_regular_file() { [[ -f "$1" && ! -L "$1" ]] || fail "file must be regular and not a symlink: $1"; }
unique_prop() {
  local key="$1"
  local file="$2"
  local values=()
  mapfile -t values < <(awk -F= -v key="$key" '$1 == key {print substr($0, index($0, "=") + 1)}' "$file")
  [[ ${#values[@]} -eq 1 ]] || fail "expected exactly one $key in $file"
  printf '%s\n' "${values[0]}"
}
normalize_sha256() {
  printf '%s' "$1" | tr -d '[:space:]:' | tr '[:upper:]' '[:lower:]'
}
resolve_apksigner() {
  local configured="${APKSIGNER:-}"
  local sdk_root="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
  local candidates=()
  if [[ -n "$configured" ]]; then
    [[ -f "$configured" && -x "$configured" && ! -L "$configured" ]] \
      || fail "APKSIGNER must be an executable regular file"
    realpath -e "$configured"
    return
  fi
  if command -v apksigner >/dev/null 2>&1; then
    configured="$(command -v apksigner)"
    [[ -x "$configured" ]] || fail "apksigner on PATH is not executable"
    realpath -e "$configured"
    return
  fi
  if [[ -n "$sdk_root" && -d "$sdk_root/build-tools" ]]; then
    mapfile -t candidates < <(
      find "$sdk_root/build-tools" -mindepth 2 -maxdepth 2 -type f -name apksigner -perm -u+x -print \
        | LC_ALL=C sort -V
    )
  fi
  [[ ${#candidates[@]} -gt 0 ]] || fail "apksigner is required to verify the APK signing certificate"
  realpath -e "${candidates[${#candidates[@]} - 1]}"
}
verify_apk_signer() {
  local apk_path="$1"
  local expected="$2"
  local apksigner_path
  local output
  local digests=()
  apksigner_path="$(resolve_apksigner)"
  output="$("$apksigner_path" verify --print-certs "$apk_path")" \
    || fail "apksigner rejected APK: $apk_path"
  mapfile -t digests < <(
    printf '%s\n' "$output" \
      | sed -nE 's/^Signer #[0-9]+ certificate SHA-256 digest:[[:space:]]*([0-9A-Fa-f:]+)[[:space:]]*$/\1/p' \
      | while IFS= read -r digest; do normalize_sha256 "$digest"; printf '\n'; done \
      | LC_ALL=C sort -u
  )
  [[ ${#digests[@]} -eq 1 && "${digests[0]}" =~ ^[0-9a-f]{64}$ ]] \
    || fail "expected exactly one APK signer SHA-256 digest"
  [[ "${digests[0]}" == "$expected" ]] \
    || fail "APK signer SHA-256 mismatch: ${digests[0]}"
  printf '%s\n' "${digests[0]}"
}

require_file "$TEMPLATE_DIR/module.prop"
require_file "$ROOT/LICENSE"
[[ "$RELEASE_KIND" == released || "$RELEASE_KIND" == candidate ]] || fail "RELEASE_KIND must be released or candidate"
if [[ "$RELEASE_KIND" == released ]]; then
  [[ -n "$EXPECTED_APK_SIGNER_SHA256" ]] \
    || fail "FREEZEIT_EXPECTED_APK_SIGNER_SHA256 is required for RELEASE_KIND=released"
fi
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
require_regular_file "$DAEMON"
require_regular_file "$APK"
DAEMON="$(realpath -e "$DAEMON")"
APK="$(realpath -e "$APK")"

readelf -h "$DAEMON" | grep -Eq 'Machine:[[:space:]]+AArch64' || fail "daemon is not an AArch64 ELF: $DAEMON"

APK_METADATA="${APK_METADATA:-$(dirname "$APK")/output-metadata.json}"
require_regular_file "$APK_METADATA"
APK_METADATA="$(realpath -e "$APK_METADATA")"

build_session_id=none
build_session_manifest_sha=none
session_daemon_sha=none
session_apk_sha=none
if [[ -n "$BUILD_SESSION_FILE" || -n "$BUILD_SESSION_ID" || -n "$BUILD_SESSION_SHA256" ]]; then
  [[ -n "$BUILD_SESSION_FILE" && -n "$BUILD_SESSION_ID" && -n "$BUILD_SESSION_SHA256" ]] \
    || fail "build session file, ID, and SHA-256 must be provided together"
  require_regular_file "$BUILD_SESSION_FILE"
  session_root="$(realpath -e "$BUILD_SESSION_ROOT")"
  session_file="$(realpath -e "$BUILD_SESSION_FILE")"
  case "$session_file" in
    "$session_root"/build-session.*/session.manifest) ;;
    *) fail "build session manifest is outside the controlled session root" ;;
  esac
  [[ "$(stat -c '%u' "$session_file")" == "$(id -u)" ]] \
    || fail "build session manifest owner mismatch"
  [[ "$(stat -c '%a' "$session_file")" == 600 ]] \
    || fail "build session manifest permissions must be 0600"
  expected_session_sha="$(normalize_sha256 "$BUILD_SESSION_SHA256")"
  [[ "$expected_session_sha" =~ ^[0-9a-f]{64}$ ]] || fail "invalid build session SHA-256"
  actual_session_sha="$(sha256sum "$session_file" | awk '{print $1}')"
  [[ "$actual_session_sha" == "$expected_session_sha" ]] || fail "build session manifest SHA-256 mismatch"
  [[ "$(unique_prop format "$session_file")" == freezeit-build-session-v1 ]] \
    || fail "unsupported build session format"
  manifest_session_id="$(unique_prop sessionId "$session_file")"
  [[ "$manifest_session_id" =~ ^[0-9a-f]{32}$ && "$manifest_session_id" == "$BUILD_SESSION_ID" ]] \
    || fail "build session ID mismatch"
  manifest_commit="$(unique_prop gitCommit "$session_file")"
  [[ "$manifest_commit" == "$(git -C "$ROOT" rev-parse HEAD)" ]] || fail "build session commit mismatch"
  [[ "$(unique_prop releaseKind "$session_file")" == "$RELEASE_KIND" ]] \
    || fail "build session release kind mismatch"
  [[ "$(unique_prop daemonPath "$session_file")" == "$DAEMON" ]] || fail "daemon path is not from this build session"
  [[ "$(unique_prop apkPath "$session_file")" == "$APK" ]] || fail "APK path is not from this build session"
  [[ "$(unique_prop apkMetadataPath "$session_file")" == "$APK_METADATA" ]] \
    || fail "APK metadata path is not from this build session"
  session_daemon_sha="$(unique_prop daemonSha256 "$session_file")"
  session_apk_sha="$(unique_prop apkSha256 "$session_file")"
  [[ "$session_daemon_sha" == "$(sha256sum "$DAEMON" | awk '{print $1}')" ]] \
    || fail "daemon was modified after the build session"
  [[ "$session_apk_sha" == "$(sha256sum "$APK" | awk '{print $1}')" ]] \
    || fail "APK was modified after the build session"
  [[ "$(unique_prop apkMetadataSha256 "$session_file")" == "$(sha256sum "$APK_METADATA" | awk '{print $1}')" ]] \
    || fail "APK metadata was modified after the build session"
  build_session_id="$manifest_session_id"
  build_session_manifest_sha="$actual_session_sha"
elif [[ "$RELEASE_KIND" == released ]]; then
  fail "RELEASE_KIND=released must be packaged by scripts/build-release.sh with a verified build session"
fi

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

expected_signer=unverified
apk_signer_sha=unverified
if [[ -n "$EXPECTED_APK_SIGNER_SHA256" ]]; then
  expected_signer="$(normalize_sha256 "$EXPECTED_APK_SIGNER_SHA256")"
  [[ "$expected_signer" =~ ^[0-9a-f]{64}$ ]] || fail "invalid FREEZEIT_EXPECTED_APK_SIGNER_SHA256"
  apk_signer_sha="$(verify_apk_signer "$APK" "$expected_signer")"
fi

for forbidden in freezeitARM64 freezeitX64 freezeitRustARM64 freezeitRustX64; do
  [[ ! -e "$TEMPLATE_DIR/$forbidden" ]] || fail "template contains forbidden daemon: $forbidden"
done
[[ ! -e "$TEMPLATE_DIR/freezeit" ]] || fail "template must not contain a daemon binary"
if find "$TEMPLATE_DIR" -maxdepth 1 -type f -name '*.apk' -print -quit | grep -q .; then
  fail "template must not contain an APK"
fi
if find "$TEMPLATE_DIR" -type l -print -quit | grep -q .; then
  fail "template must not contain symlinks"
fi
if find "$TEMPLATE_DIR" -mindepth 2 -print -quit | grep -q .; then
  fail "template must contain only top-level files"
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
if [[ "$build_session_id" != none ]]; then
  [[ "$daemon_sha" == "$session_daemon_sha" ]] || fail "staged daemon differs from the verified build session"
  [[ "$apk_sha" == "$session_apk_sha" ]] || fail "staged APK differs from the verified build session"
fi
patch_sha=none
snapshot_sha=none
state_sha=none
if [[ "$dirty" == true ]]; then
  patch_sha="$(sha256sum "$stage/source.patch" | awk '{print $1}')"
  snapshot_sha="$(sha256sum "$stage/source-snapshot.tar.gz" | awk '{print $1}')"
  state_sha="$(sha256sum "$stage/source-state.txt" | awk '{print $1}')"
fi
cat >"$stage/provenance.txt" <<PROVENANCE
format=freezeit-release-provenance-v2
version=$EXPECTED_VERSION
versionCode=$EXPECTED_VERSION_CODE
gitCommit=$commit
releaseKind=$RELEASE_KIND
dirty=$dirty
buildSessionId=$build_session_id
buildSessionManifestSha256=$build_session_manifest_sha
daemonSource=freezeitDaemon
managerSource=freezeitApp
daemonTarget=aarch64-linux-android
daemonSha256=$daemon_sha
apkSha256=$apk_sha
apkSignerSha256=$apk_signer_sha
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
