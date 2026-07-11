#!/usr/bin/env bash
set -euo pipefail

ZIP_PATH="${1:-}"
EXPECTED_VERSION="${2:-3.3.1SelfUse}"
EXPECTED_VERSION_CODE="${3:-303001}"
[[ -n "$ZIP_PATH" ]] || { echo "usage: $0 <release.zip> [version] [versionCode]" >&2; exit 2; }
[[ -f "$ZIP_PATH" ]] || { echo "release zip not found: $ZIP_PATH" >&2; exit 1; }

fail() { echo "release zip validation failed: $*" >&2; exit 1; }
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
        raw = info.filename.replace("\\", "/")
        path = pathlib.PurePosixPath(raw)
        normalized = str(path)
        if raw.startswith("/") or any(part in ("", ".", "..") for part in path.parts):
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
grep -Fx 'daemonSource=freezeitDaemon' "$stage/provenance.txt" >/dev/null || fail "missing Rust daemon provenance"
grep -Fx 'daemonTarget=aarch64-linux-android' "$stage/provenance.txt" >/dev/null || fail "missing ARM64 target provenance"
daemon_sha="$(sha256sum "$stage/freezeit" | awk '{print $1}')"
apk_sha="$(sha256sum "$stage/freezeit.apk" | awk '{print $1}')"
grep -Fx "daemonSha256=$daemon_sha" "$stage/provenance.txt" >/dev/null || fail "daemon provenance digest mismatch"
grep -Fx "apkSha256=$apk_sha" "$stage/provenance.txt" >/dev/null || fail "APK provenance digest mismatch"
if [[ "$dirty_value" == true ]]; then
  grep -Fx 'releaseKind=candidate' "$stage/provenance.txt" >/dev/null || fail "dirty package must be candidate"
  for pair in 'source.patch sourcePatchSha256' 'source-snapshot.tar.gz sourceSnapshotSha256' 'source-state.txt sourceStateSha256'; do
    set -- $pair
    digest="$(sha256sum "$stage/$1" | awk '{print $1}')"
    grep -Fx "$2=$digest" "$stage/provenance.txt" >/dev/null || fail "$1 provenance digest mismatch"
  done
else
  grep -Fx 'releaseKind=released' "$stage/provenance.txt" >/dev/null || fail "clean package must be released"
  grep -Fx 'sourcePatchSha256=none' "$stage/provenance.txt" >/dev/null || fail "clean package patch digest must be none"
  grep -Fx 'sourceSnapshotSha256=none' "$stage/provenance.txt" >/dev/null || fail "clean package snapshot digest must be none"
fi
grep -Eq '^https://github\.com/szh1118/freezeit_cos16/tree/[0-9a-f]{40}$' "$stage/SOURCE_OFFER" || fail "missing source commit URL"
grep -Fx "version=$EXPECTED_VERSION" "$stage/module.prop" >/dev/null || fail "module version mismatch"
grep -Fx "versionCode=$EXPECTED_VERSION_CODE" "$stage/module.prop" >/dev/null || fail "module versionCode mismatch"

echo "release zip validation: pass"
