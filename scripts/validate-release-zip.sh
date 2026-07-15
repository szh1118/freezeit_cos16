#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
module_prop() {
  awk -F= -v key="$1" '$1 == key {print substr($0, index($0, "=") + 1); exit}' "$ROOT/magisk/module.prop"
}
ZIP_PATH="${1:-}"
EXPECTED_VERSION="${2:-$(module_prop version)}"
EXPECTED_VERSION_CODE="${3:-$(module_prop versionCode)}"
SOURCE_REPOSITORY_URL="${SOURCE_REPOSITORY_URL:-https://github.com/szh1118/freezeit_cos16}"
SOURCE_REPOSITORY_URL="${SOURCE_REPOSITORY_URL%/}"
EXPECTED_BUILD_SESSION_MANIFEST_SHA256="${FREEZEIT_EXPECTED_BUILD_SESSION_MANIFEST_SHA256:-}"
MAX_ARCHIVE_ENTRY_BYTES="${FREEZEIT_MAX_RELEASE_ENTRY_BYTES:-67108864}"
MAX_ARCHIVE_UNCOMPRESSED_BYTES="${FREEZEIT_MAX_RELEASE_UNCOMPRESSED_BYTES:-134217728}"
[[ -n "$ZIP_PATH" ]] || { echo "usage: $0 <release.zip> [version] [versionCode]" >&2; exit 2; }
[[ -f "$ZIP_PATH" ]] || { echo "release zip not found: $ZIP_PATH" >&2; exit 1; }

fail() { echo "release zip validation failed: $*" >&2; exit 1; }
unique_prop() {
  local key="$1"
  local file="$2"
  local values=()
  mapfile -t values < <(awk -F= -v key="$key" '$1 == key {print substr($0, index($0, "=") + 1)}' "$file")
  [[ ${#values[@]} -eq 1 ]] || fail "expected exactly one $key in $(basename "$file")"
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
  if [[ -z "$sdk_root" && -d "$HOME/Android/Sdk" ]]; then
    sdk_root="$HOME/Android/Sdk"
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
apk_signer_sha256() {
  local apk_path="$1"
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
  printf '%s\n' "${digests[0]}"
}
validate_android_aarch64_executable() {
  local binary="$1"
  local header
  local program_headers
  local entry_point
  local elf_type
  local interpreter_segments
  local dynamic_segments
  local entry_point_value
  local load_address
  local load_size
  local executable_entry=false
  local interpreters=()
  header="$(readelf -h "$binary")" || fail "freezeit is not an ELF"
  printf '%s\n' "$header" | grep -Eq 'Class:[[:space:]]+ELF64' \
    || fail "freezeit is not a 64-bit ELF"
  printf '%s\n' "$header" | grep -Eq 'Data:[[:space:]]+2.s complement, little endian' \
    || fail "freezeit is not a little-endian ELF"
  elf_type="$(awk '/^[[:space:]]*Type:/ {print $2; exit}' <<<"$header")"
  [[ "$elf_type" == DYN || "$elf_type" == EXEC ]] || fail "freezeit is not an executable ELF"
  printf '%s\n' "$header" | grep -Eq 'Machine:[[:space:]]+AArch64' \
    || fail "freezeit is not an AArch64 ELF"
  entry_point="$(awk '/Entry point address:/ {print $4}' <<<"$header")"
  [[ "$entry_point" =~ ^0x[0-9A-Fa-f]+$ && ! "$entry_point" =~ ^0x0+$ ]] \
    || fail "freezeit has no executable entry point"
  program_headers="$(readelf -W -l "$binary")" || fail "cannot inspect freezeit program headers"
  entry_point_value=$((16#${entry_point#0x}))
  while IFS=' ' read -r load_address load_size; do
    [[ "$load_address" =~ ^0x[0-9A-Fa-f]+$ && "$load_size" =~ ^0x[0-9A-Fa-f]+$ ]] || continue
    if (( load_size > 0 && entry_point_value >= load_address && entry_point_value - load_address < load_size )); then
      executable_entry=true
      break
    fi
  done < <(
    awk '$1 == "LOAD" && ($7 ~ /E/ || $8 ~ /E/) {print $3, $6}' <<<"$program_headers"
  )
  [[ "$executable_entry" == true ]] || fail "freezeit has no executable PT_LOAD segment"
  interpreter_segments="$(awk '$1 == "INTERP" {count++} END {print count + 0}' <<<"$program_headers")"
  dynamic_segments="$(awk '$1 == "DYNAMIC" {count++} END {print count + 0}' <<<"$program_headers")"
  mapfile -t interpreters < <(
    printf '%s\n' "$program_headers" \
      | sed -nE 's/^[[:space:]]*\[Requesting program interpreter: ([^][]+)\][[:space:]]*$/\1/p'
  )
  if [[ "$elf_type" == DYN ]]; then
    [[ "$interpreter_segments" -eq 1 && ${#interpreters[@]} -eq 1 ]] \
      || fail "freezeit must have exactly one Android PT_INTERP"
    [[ "${interpreters[0]}" == /system/bin/linker64 ]] \
      || fail "freezeit is not linked for the Android AArch64 dynamic linker"
    [[ "$dynamic_segments" -eq 1 ]] || fail "dynamic freezeit must have exactly one PT_DYNAMIC"
  else
    [[ "$interpreter_segments" -eq 0 && ${#interpreters[@]} -eq 0 ]] \
      || fail "static freezeit must not have PT_INTERP"
    [[ "$dynamic_segments" -eq 0 ]] || fail "static freezeit must not have PT_DYNAMIC"
  fi
  return 0
}
[[ "$EXPECTED_VERSION" =~ ^[0-9A-Za-z][0-9A-Za-z._+-]{0,63}$ ]] || fail "invalid expected version"
[[ "$EXPECTED_VERSION_CODE" =~ ^[0-9]{1,10}$ ]] || fail "invalid expected versionCode"
[[ "$MAX_ARCHIVE_ENTRY_BYTES" =~ ^[1-9][0-9]*$ ]] || fail "invalid FREEZEIT_MAX_RELEASE_ENTRY_BYTES"
[[ "$MAX_ARCHIVE_UNCOMPRESSED_BYTES" =~ ^[1-9][0-9]*$ ]] || fail "invalid FREEZEIT_MAX_RELEASE_UNCOMPRESSED_BYTES"

python3 - "$ZIP_PATH" "$MAX_ARCHIVE_ENTRY_BYTES" "$MAX_ARCHIVE_UNCOMPRESSED_BYTES" <<'PY' || fail "unsafe archive structure"
import pathlib
import stat
import sys
import zipfile

archive_path, entry_limit, total_limit = sys.argv[1:]
entry_limit = int(entry_limit)
total_limit = int(total_limit)
seen = set()
total_size = 0
with zipfile.ZipFile(archive_path) as archive:
    for info in archive.infolist():
        raw = info.filename
        if "\\" in raw or any(ord(character) < 32 or ord(character) == 127 for character in raw):
            raise SystemExit(f"unsafe path characters: {raw!r}")
        path = pathlib.PurePosixPath(raw)
        normalized = str(path)
        if (
            info.is_dir()
            or raw != normalized
            or raw.startswith("/")
            or len(path.parts) != 1
            or any(part in ("", ".", "..") for part in path.parts)
        ):
            raise SystemExit(f"unsafe path: {raw}")
        if normalized in seen:
            raise SystemExit(f"duplicate path: {normalized}")
        seen.add(normalized)
        mode = info.external_attr >> 16
        if stat.S_ISLNK(mode):
            raise SystemExit(f"symlink entry: {normalized}")
        if info.file_size > entry_limit:
            raise SystemExit(f"entry exceeds uncompressed size limit: {normalized}")
        total_size += info.file_size
        if total_size > total_limit:
            raise SystemExit("archive exceeds total uncompressed size limit")
PY

mapfile -t entries < <(unzip -Z1 "$ZIP_PATH" | sed '/\/$/d')
count_entry() { printf '%s\n' "${entries[@]}" | grep -Fxc "$1" || true; }
require_entry() { [[ "$(count_entry "$1")" -eq 1 ]] || fail "expected one $1 entry"; }

for entry in module.prop customize.sh service.sh uninstall.sh rom_baseline.prop verified_targets.txt changelog.txt freezeit freezeit.apk LICENSE SOURCE_OFFER SHA256SUMS provenance.txt; do
  require_entry "$entry"
done
dirty_value="$(unzip -p "$ZIP_PATH" provenance.txt | awk -F= '$1 == "dirty" {print $2; exit}')"
base_allowlist=(module.prop customize.sh service.sh uninstall.sh rom_baseline.prop verified_targets.txt changelog.txt freezeit freezeit.apk LICENSE SOURCE_OFFER SHA256SUMS provenance.txt build-session.manifest appcfg.txt applabel.txt skip_mount system.prop)
if [[ "$dirty_value" == true ]]; then
  base_allowlist+=(source.patch source-snapshot.tar.gz source-state.txt)
elif [[ "$dirty_value" != false ]]; then
  fail "provenance dirty must be true or false"
fi
for entry in "${entries[@]}"; do
  allowed=false
  for candidate in "${base_allowlist[@]}"; do
    [[ "$entry" == "$candidate" ]] && allowed=true && break
  done
  [[ "$allowed" == true ]] || fail "entry is not allowlisted: $entry"
done
[[ "$(printf '%s\n' "${entries[@]}" | grep -Ec '^freezeit$')" -eq 1 ]] || fail "archive must contain exactly one daemon"
if printf '%s\n' "${entries[@]}" | grep -Eq '(^|/)(freezeitARM64|freezeitX64|freezeitRustARM64|freezeitRustX64)$'; then
  fail "archive contains a forbidden legacy daemon"
fi
[[ "$(printf '%s\n' "${entries[@]}" | grep -Ec '(^|/).*\.apk$')" -eq 1 ]] || fail "archive must contain exactly one APK"

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
unzip -q "$ZIP_PATH" -d "$stage"
validate_android_aarch64_executable "$stage/freezeit"
mapfile -t elf_entries < <(
  for entry in "${entries[@]}"; do
    [[ -f "$stage/$entry" ]] || continue
    readelf -h "$stage/$entry" >/dev/null 2>&1 && printf '%s\n' "$entry"
  done
)
[[ ${#elf_entries[@]} -eq 1 && "${elf_entries[0]}" == freezeit ]] || fail "archive must contain exactly one ELF daemon"
(
  cd "$stage"
  sha256sum -c SHA256SUMS >/dev/null
) || fail "SHA256SUMS verification failed"
mapfile -t hashed_entries < <(sed -E 's/^[0-9a-fA-F]{64}[[:space:]]+[*]?//' "$stage/SHA256SUMS" | LC_ALL=C sort)
mapfile -t expected_hashed_entries < <(printf '%s\n' "${entries[@]}" | grep -Fxv SHA256SUMS | LC_ALL=C sort)
[[ "${hashed_entries[*]}" == "${expected_hashed_entries[*]}" ]] || fail "SHA256SUMS must cover every archive file except itself"

grep -Fx "version=$EXPECTED_VERSION" "$stage/provenance.txt" >/dev/null || fail "provenance version mismatch"
grep -Fx "versionCode=$EXPECTED_VERSION_CODE" "$stage/provenance.txt" >/dev/null || fail "provenance versionCode mismatch"
[[ "$(unique_prop format "$stage/provenance.txt")" == freezeit-release-provenance-v2 ]] \
  || fail "unsupported provenance format"
release_kind="$(unique_prop releaseKind "$stage/provenance.txt")"
[[ "$release_kind" == released || "$release_kind" == candidate ]] || fail "invalid release kind"
dirty_value="$(unique_prop dirty "$stage/provenance.txt")"
grep -Fx 'daemonSource=freezeitDaemon' "$stage/provenance.txt" >/dev/null || fail "missing Rust daemon provenance"
grep -Fx 'daemonTarget=aarch64-linux-android' "$stage/provenance.txt" >/dev/null || fail "missing ARM64 target provenance"
daemon_sha="$(sha256sum "$stage/freezeit" | awk '{print $1}')"
apk_sha="$(sha256sum "$stage/freezeit.apk" | awk '{print $1}')"
grep -Fx "daemonSha256=$daemon_sha" "$stage/provenance.txt" >/dev/null || fail "daemon provenance digest mismatch"
grep -Fx "apkSha256=$apk_sha" "$stage/provenance.txt" >/dev/null || fail "APK provenance digest mismatch"
apk_signer_sha="$(unique_prop apkSignerSha256 "$stage/provenance.txt")"
if [[ "$apk_signer_sha" != unverified && ! "$apk_signer_sha" =~ ^[0-9a-f]{64}$ ]]; then
  fail "invalid APK signer SHA-256 provenance"
fi
if [[ "$apk_signer_sha" != unverified || -n "${FREEZEIT_EXPECTED_APK_SIGNER_SHA256:-}" ]]; then
  [[ "$apk_signer_sha" =~ ^[0-9a-f]{64}$ ]] \
    || fail "APK signer provenance is not verified"
  actual_apk_signer_sha="$(apk_signer_sha256 "$stage/freezeit.apk")"
  [[ "$actual_apk_signer_sha" == "$apk_signer_sha" ]] \
    || fail "APK signer does not match provenance"
fi
if [[ -n "${FREEZEIT_EXPECTED_APK_SIGNER_SHA256:-}" ]]; then
  expected_signer="$(normalize_sha256 "$FREEZEIT_EXPECTED_APK_SIGNER_SHA256")"
  [[ "$expected_signer" =~ ^[0-9a-f]{64}$ ]] || fail "invalid expected APK signer SHA-256"
  [[ "$actual_apk_signer_sha" == "$expected_signer" ]] || fail "APK signer does not match expected signer"
fi
if [[ "$dirty_value" == true ]]; then
  [[ "$release_kind" == candidate ]] || fail "dirty package must be candidate"
  for pair in 'source.patch sourcePatchSha256' 'source-snapshot.tar.gz sourceSnapshotSha256' 'source-state.txt sourceStateSha256'; do
    set -- $pair
    digest="$(sha256sum "$stage/$1" | awk '{print $1}')"
    grep -Fx "$2=$digest" "$stage/provenance.txt" >/dev/null || fail "$1 provenance digest mismatch"
  done
else
  [[ "$dirty_value" == false ]] || fail "provenance dirty must be true or false"
  grep -Fx 'sourcePatchSha256=none' "$stage/provenance.txt" >/dev/null || fail "clean package patch digest must be none"
  grep -Fx 'sourceSnapshotSha256=none' "$stage/provenance.txt" >/dev/null || fail "clean package snapshot digest must be none"
  grep -Fx 'sourceStateSha256=none' "$stage/provenance.txt" >/dev/null || fail "clean package state digest must be none"
fi
build_session_id="$(unique_prop buildSessionId "$stage/provenance.txt")"
build_session_manifest_sha="$(unique_prop buildSessionManifestSha256 "$stage/provenance.txt")"
git_commit="$(unique_prop gitCommit "$stage/provenance.txt")"
[[ "$git_commit" =~ ^[0-9a-f]{40}$ ]] || fail "invalid provenance git commit"
session_entry_count="$(count_entry build-session.manifest)"
if [[ "$build_session_id" == none || "$build_session_manifest_sha" == none ]]; then
  [[ "$build_session_id" == none && "$build_session_manifest_sha" == none ]] \
    || fail "build session provenance must be complete"
  [[ "$session_entry_count" -eq 0 ]] || fail "archive has an unexpected build session manifest"
else
  [[ "$build_session_id" =~ ^[0-9a-f]{32}$ ]] || fail "invalid build session ID"
  [[ "$build_session_manifest_sha" =~ ^[0-9a-f]{64}$ ]] \
    || fail "invalid build session manifest digest"
  [[ "$session_entry_count" -eq 1 ]] || fail "archive is missing its build session manifest"
  actual_session_manifest_sha="$(sha256sum "$stage/build-session.manifest" | awk '{print $1}')"
  [[ "$actual_session_manifest_sha" == "$build_session_manifest_sha" ]] \
    || fail "build session manifest digest mismatch"
  [[ "$(unique_prop format "$stage/build-session.manifest")" == freezeit-build-session-v1 ]] \
    || fail "unsupported build session manifest format"
  [[ "$(unique_prop sessionId "$stage/build-session.manifest")" == "$build_session_id" ]] \
    || fail "build session manifest ID mismatch"
  [[ "$(unique_prop gitCommit "$stage/build-session.manifest")" == "$git_commit" ]] \
    || fail "build session manifest commit mismatch"
  [[ "$(unique_prop releaseKind "$stage/build-session.manifest")" == "$release_kind" ]] \
    || fail "build session manifest release kind mismatch"
  [[ "$(unique_prop version "$stage/build-session.manifest")" == "$EXPECTED_VERSION" ]] \
    || fail "build session manifest version mismatch"
  [[ "$(unique_prop versionCode "$stage/build-session.manifest")" == "$EXPECTED_VERSION_CODE" ]] \
    || fail "build session manifest versionCode mismatch"
  [[ "$(unique_prop daemonSha256 "$stage/build-session.manifest")" == "$daemon_sha" ]] \
    || fail "build session manifest daemon digest mismatch"
  [[ "$(unique_prop apkSha256 "$stage/build-session.manifest")" == "$apk_sha" ]] \
    || fail "build session manifest APK digest mismatch"
  manifest_metadata_sha="$(unique_prop apkMetadataSha256 "$stage/build-session.manifest")"
  [[ "$manifest_metadata_sha" =~ ^[0-9a-f]{64}$ ]] \
    || fail "invalid build session manifest APK metadata digest"
  [[ -n "$(unique_prop daemonPath "$stage/build-session.manifest")" ]] \
    || fail "build session manifest daemon path is empty"
  [[ -n "$(unique_prop apkPath "$stage/build-session.manifest")" ]] \
    || fail "build session manifest APK path is empty"
  [[ -n "$(unique_prop apkMetadataPath "$stage/build-session.manifest")" ]] \
    || fail "build session manifest APK metadata path is empty"
fi
if [[ "$release_kind" == released ]]; then
  [[ "$dirty_value" == false ]] || fail "released package must be clean"
  [[ "$build_session_id" != none && "$build_session_manifest_sha" != none ]] \
    || fail "released package is missing a verified build session manifest"
  [[ "$apk_signer_sha" =~ ^[0-9a-f]{64}$ ]] || fail "released package requires a verified APK signer"
  expected_session_manifest_sha="$(normalize_sha256 "$EXPECTED_BUILD_SESSION_MANIFEST_SHA256")"
  [[ "$expected_session_manifest_sha" =~ ^[0-9a-f]{64}$ ]] \
    || fail "released package requires FREEZEIT_EXPECTED_BUILD_SESSION_MANIFEST_SHA256"
  [[ "$actual_session_manifest_sha" == "$expected_session_manifest_sha" ]] \
    || fail "build session manifest does not match the trusted build session manifest digest"
fi
grep -Fx "$SOURCE_REPOSITORY_URL/tree/$git_commit" "$stage/SOURCE_OFFER" >/dev/null \
  || fail "missing matching source commit URL"
grep -Fx "version=$EXPECTED_VERSION" "$stage/module.prop" >/dev/null || fail "module version mismatch"
grep -Fx "versionCode=$EXPECTED_VERSION_CODE" "$stage/module.prop" >/dev/null || fail "module versionCode mismatch"

echo "release zip validation: pass"
