#!/usr/bin/env bash
set -euo pipefail

ZIP_PATH="${1:-}"
EXPECTED_VERSION="${2:-3.3.2SelfUse}"
EXPECTED_VERSION_CODE="${3:-303002}"
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
[[ "$EXPECTED_VERSION" =~ ^[0-9A-Za-z][0-9A-Za-z._+-]{0,63}$ ]] || fail "invalid expected version"
[[ "$EXPECTED_VERSION_CODE" =~ ^[0-9]{1,10}$ ]] || fail "invalid expected versionCode"

python3 - "$ZIP_PATH" <<'PY' || fail "unsafe archive structure"
import pathlib
import stat
import sys
import zipfile

seen = set()
with zipfile.ZipFile(sys.argv[1]) as archive:
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
PY

mapfile -t entries < <(unzip -Z1 "$ZIP_PATH" | sed '/\/$/d')
count_entry() { printf '%s\n' "${entries[@]}" | grep -Fxc "$1" || true; }
require_entry() { [[ "$(count_entry "$1")" -eq 1 ]] || fail "expected one $1 entry"; }

for entry in module.prop customize.sh service.sh uninstall.sh rom_baseline.prop verified_targets.txt changelog.txt freezeit freezeit.apk LICENSE SOURCE_OFFER SHA256SUMS provenance.txt; do
  require_entry "$entry"
done
dirty_value="$(unzip -p "$ZIP_PATH" provenance.txt | awk -F= '$1 == "dirty" {print $2; exit}')"
base_allowlist=(module.prop customize.sh service.sh uninstall.sh rom_baseline.prop verified_targets.txt changelog.txt freezeit freezeit.apk LICENSE SOURCE_OFFER SHA256SUMS provenance.txt appcfg.txt applabel.txt skip_mount system.prop)
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
readelf -h "$stage/freezeit" | grep -Eq 'Machine:[[:space:]]+AArch64' || fail "freezeit is not an AArch64 ELF"
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
if [[ -n "${FREEZEIT_EXPECTED_APK_SIGNER_SHA256:-}" ]]; then
  expected_signer="$(normalize_sha256 "$FREEZEIT_EXPECTED_APK_SIGNER_SHA256")"
  [[ "$expected_signer" =~ ^[0-9a-f]{64}$ ]] || fail "invalid expected APK signer SHA-256"
  [[ "$apk_signer_sha" == "$expected_signer" ]] || fail "APK signer provenance does not match expected signer"
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
if [[ "$release_kind" == released ]]; then
  [[ "$dirty_value" == false ]] || fail "released package must be clean"
  [[ "$build_session_id" =~ ^[0-9a-f]{32}$ ]] || fail "released package is missing a valid build session ID"
  [[ "$build_session_manifest_sha" =~ ^[0-9a-f]{64}$ ]] \
    || fail "released package is missing a valid build session manifest digest"
  [[ "$apk_signer_sha" =~ ^[0-9a-f]{64}$ ]] || fail "released package requires a verified APK signer"
else
  if [[ "$build_session_id" == none || "$build_session_manifest_sha" == none ]]; then
    [[ "$build_session_id" == none && "$build_session_manifest_sha" == none ]] \
      || fail "candidate build session provenance must be complete"
  else
    [[ "$build_session_id" =~ ^[0-9a-f]{32}$ ]] || fail "invalid candidate build session ID"
    [[ "$build_session_manifest_sha" =~ ^[0-9a-f]{64}$ ]] \
      || fail "invalid candidate build session manifest digest"
  fi
fi
git_commit="$(unique_prop gitCommit "$stage/provenance.txt")"
[[ "$git_commit" =~ ^[0-9a-f]{40}$ ]] || fail "invalid provenance git commit"
grep -Fx "https://github.com/szh1118/freezeit_cos16/tree/$git_commit" "$stage/SOURCE_OFFER" >/dev/null \
  || fail "missing matching source commit URL"
grep -Fx "version=$EXPECTED_VERSION" "$stage/module.prop" >/dev/null || fail "module version mismatch"
grep -Fx "versionCode=$EXPECTED_VERSION_CODE" "$stage/module.prop" >/dev/null || fail "module versionCode mismatch"

echo "release zip validation: pass"
