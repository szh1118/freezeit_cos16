#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fail() { echo "release pipeline test failed: $*" >&2; exit 1; }
module_prop() {
  awk -F= -v key="$1" '$1 == key {print substr($0, index($0, "=") + 1); exit}' "$ROOT/magisk/module.prop"
}
expected_version="$(module_prop version)"
expected_code="$(module_prop versionCode)"
[[ -n "$expected_version" && -n "$expected_code" ]] || fail "module version metadata is missing"
original_apksigner="${APKSIGNER-}"
original_expected_apk_signer_sha="${FREEZEIT_EXPECTED_APK_SIGNER_SHA256-}"
original_expected_session_manifest_sha="${FREEZEIT_EXPECTED_BUILD_SESSION_MANIFEST_SHA256-}"

make_android_aarch64_elf() {
  local output="$1"
  cp /bin/true "$output"
  python3 - "$output" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
data = bytearray(path.read_bytes())
data[18:20] = b"\xb7\x00"  # EM_AARCH64 in the ELF header.
old = b"/lib64/ld-linux-x86-64.so.2\x00"
new = b"/system/bin/linker64\x00"
offset = data.find(old)
if offset < 0:
    raise SystemExit("host fixture has no Linux dynamic linker path")
data[offset:offset + len(old)] = new + b"\x00" * (len(old) - len(new))
path.write_bytes(data)
PY
}

make_static_aarch64_elf() {
  local output="$1"
  python3 - "$output" <<'PY'
import struct
import sys

header = bytearray(64)
header[:16] = b"\x7fELF\x02\x01\x01\x00" + b"\x00" * 8
struct.pack_into("<HHI", header, 16, 2, 183, 1)  # ET_EXEC, EM_AARCH64, EV_CURRENT.
struct.pack_into("<QQQ", header, 24, 0x10000, 64, 0)
struct.pack_into("<IHHHHHH", header, 48, 0, 64, 56, 1, 0, 0, 0)
program_header = struct.pack("<IIQQQQQQ", 1, 5, 0x1000, 0x10000, 0x10000, 4, 4, 0x1000)
with open(sys.argv[1], "wb") as stream:
    stream.write(header)
    stream.write(program_header)
    stream.write(b"\x00" * (0x1000 - 64 - len(program_header)))
    stream.write(b"\xc0\x03\x5f\xd6")  # AArch64 ret.
PY
}

[[ -d "$ROOT/magisk" ]] || fail "top-level magisk template missing"
for forbidden in freezeit freezeitARM64 freezeitX64 freezeitRustARM64 freezeitRustX64; do
  [[ ! -e "$ROOT/magisk/$forbidden" ]] || fail "template contains daemon: $forbidden"
done
if find "$ROOT/magisk" -maxdepth 1 -type f -name '*.apk' -print -quit | grep -q .; then
  fail "template contains APK"
fi
grep -Fx "version=$expected_version" "$ROOT/magisk/module.prop" >/dev/null || fail "planned version missing"
grep -Fx "versionCode=$expected_code" "$ROOT/magisk/module.prop" >/dev/null || fail "planned versionCode missing"
grep -F '仅支持 ARM64' "$ROOT/magisk/customize.sh" >/dev/null || fail "installer does not reject non-ARM64"
grep -F 'freezeitARM64 freezeitX64' "$ROOT/magisk/customize.sh" >/dev/null || fail "legacy daemon rejection missing"
grep -F '检测到 [NoANR]' "$ROOT/magisk/customize.sh" >/dev/null || fail "NoANR conflict warning missing"
if grep -F 'pm uninstall cn.myflv.android.noanr' "$ROOT/magisk/customize.sh" >/dev/null; then
  fail "installer still uninstalls NoANR"
fi
grep -F 'expected exactly one APK named freezeit.apk' "$ROOT/magisk/customize.sh" >/dev/null \
  || fail "installer APK uniqueness gate missing"
grep -F 'abort "- 🚫 冻它APP 覆盖安装失败' "$ROOT/magisk/customize.sh" >/dev/null \
  || fail "installer does not abort after preserving a failed APK update"
grep -F 'RELEASE_KIND=released requires FREEZEIT_KEYSTORE' "$ROOT/freezeitApp/app/build.gradle" >/dev/null \
  || fail "Gradle released-signing gate missing"
grep -F 'initWith signingConfigs.debug' "$ROOT/freezeitApp/app/build.gradle" >/dev/null \
  || fail "candidate debug-signing fallback missing"
grep -F 'FREEZEIT_BUILD_SESSION_FILE="$session_file"' "$ROOT/scripts/build-release.sh" >/dev/null \
  || fail "build-release does not pass its build session to the packager"
grep -F 'freezeitVS/magisk' "$ROOT/scripts/package-release.sh" >/dev/null && fail "packager still uses legacy template"
grep -F 'freezeitARM64' "$ROOT/scripts/package-release.sh" >/dev/null || fail "packager does not reject legacy daemon names"
grep -F 'working tree is dirty' "$ROOT/scripts/package-release.sh" >/dev/null || fail "packager does not reject dirty trees by default"
grep -F 'FREEZEIT_BUILD_SESSION_FILE' "$ROOT/scripts/package-release.sh" >/dev/null \
  || fail "released build-session gate missing"
grep -F 'FREEZEIT_EXPECTED_APK_SIGNER_SHA256 is required' "$ROOT/scripts/package-release.sh" >/dev/null \
  || fail "released APK signer gate missing"
grep -F 'source-snapshot.tar.gz' "$ROOT/scripts/package-release.sh" >/dev/null || fail "dirty candidate source snapshot is missing"
grep -F '[[ -e "$ROOT/$path" || -L "$ROOT/$path" ]]' "$ROOT/scripts/package-release.sh" >/dev/null || fail "dirty candidate snapshot does not skip deleted paths"
grep -F 'sourcePatchSha256' "$ROOT/scripts/package-release.sh" >/dev/null || fail "dirty candidate patch digest is missing"
if "$ROOT/scripts/validate-release-zip.sh" /nonexistent/freezeit.zip >/dev/null 2>&1; then
  fail "validator accepted a missing zip"
fi

tmp="$(mktemp -d)"
metadata_path="$tmp-output-metadata.json"
apksigner_path="$tmp-apksigner"
metadata_root="${tmp}-metadata-root"
relative_out_invocation="${tmp}-relative-out-invocation"
relative_out_stage="${tmp}-relative-out-stage"
dirty_marker="$ROOT/.release-pipeline-dirty-test.$$"
trap 'rm -rf "$tmp" "$metadata_root" "$relative_out_invocation" "$relative_out_stage"; rm -f "$metadata_path" "$apksigner_path" "$dirty_marker"' EXIT
cp -a "$ROOT/magisk/." "$tmp/"
make_android_aarch64_elf "$tmp/freezeit"
printf 'test apk\n' >"$tmp/freezeit.apk"
cp "$ROOT/LICENSE" "$tmp/LICENSE"
cat >"$metadata_path" <<EOF
{"elements":[{"outputFile":"freezeit.apk","versionName":"$expected_version","versionCode":$expected_code}]}
EOF
cat >"$apksigner_path" <<'EOF'
#!/usr/bin/env sh
set -eu
printf '%s\n' 'Signer #1 certificate SHA-256 digest: bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'
EOF
chmod 0755 "$apksigner_path"
export APKSIGNER="$apksigner_path"
export FREEZEIT_EXPECTED_APK_SIGNER_SHA256=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
daemon_sha="$(sha256sum "$tmp/freezeit" | awk '{print $1}')"
apk_sha="$(sha256sum "$tmp/freezeit.apk" | awk '{print $1}')"
metadata_sha="$(sha256sum "$metadata_path" | awk '{print $1}')"
cat >"$tmp/build-session.manifest" <<EOF
format=freezeit-build-session-v1
sessionId=0123456789abcdef0123456789abcdef
gitCommit=0000000000000000000000000000000000000000
releaseKind=released
version=$expected_version
versionCode=$expected_code
daemonPath=/controlled/build/freezeit
daemonSha256=$daemon_sha
apkPath=/controlled/build/freezeit.apk
apkSha256=$apk_sha
apkMetadataPath=/controlled/build/output-metadata.json
apkMetadataSha256=$metadata_sha
EOF
session_sha="$(sha256sum "$tmp/build-session.manifest" | awk '{print $1}')"
trusted_session_sha="$session_sha"
export FREEZEIT_EXPECTED_BUILD_SESSION_MANIFEST_SHA256="$trusted_session_sha"
: >"$dirty_marker"
if build_guard_output="$(RELEASE_KIND=candidate ALLOW_DIRTY=0 "$ROOT/scripts/build-release.sh" 2>&1)"; then
  fail "build-release accepted a deliberately dirty candidate without ALLOW_DIRTY=1"
fi
grep -F 'only RELEASE_KIND=candidate ALLOW_DIRTY=1 may build it' <<<"$build_guard_output" >/dev/null \
  || fail "dirty build rejection did not happen before artifact builds"
if dirty_guard_output="$(
  RELEASE_KIND=candidate \
  ALLOW_DIRTY=0 \
  DAEMON="$tmp/freezeit" \
  APK="$tmp/freezeit.apk" \
  APK_METADATA="$metadata_path" \
  EXPECTED_VERSION="$expected_version" \
  EXPECTED_VERSION_CODE="$expected_code" \
  OUT_DIR="$tmp-out" \
  "$ROOT/scripts/package-release.sh" 2>&1
)"; then
  fail "packager accepted a deliberately dirty candidate without ALLOW_DIRTY=1"
fi
rm -f "$dirty_marker"
grep -F 'working tree is dirty' <<<"$dirty_guard_output" >/dev/null \
  || fail "dirty candidate rejection did not come from the dirty-tree guard"
mkdir -p "$relative_out_invocation"
(
  cd "$relative_out_invocation"
  unset EXPECTED_VERSION EXPECTED_VERSION_CODE
  RELEASE_KIND=candidate \
  ALLOW_DIRTY=1 \
  DAEMON="$tmp/freezeit" \
  APK="$tmp/freezeit.apk" \
  APK_METADATA="$metadata_path" \
  OUT_DIR=dist \
  STAGING_ROOT="$relative_out_stage" \
  "$ROOT/scripts/package-release.sh" >/dev/null
)
[[ -f "$relative_out_invocation/dist/freezeit_oneplus13_android16_selfuse_v${expected_version}_${expected_code}.zip" ]] \
  || fail "packager did not write a relative OUT_DIR beneath the invocation directory"
cat >"$tmp/provenance.txt" <<EOF
format=freezeit-release-provenance-v2
version=$expected_version
versionCode=$expected_code
gitCommit=0000000000000000000000000000000000000000
releaseKind=released
dirty=false
buildSessionId=0123456789abcdef0123456789abcdef
buildSessionManifestSha256=$session_sha
daemonSource=freezeitDaemon
managerSource=freezeitApp
daemonTarget=aarch64-linux-android
daemonSha256=$daemon_sha
apkSha256=$apk_sha
apkSignerSha256=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
sourcePatchSha256=none
sourceSnapshotSha256=none
sourceStateSha256=none
EOF
cat >"$tmp/SOURCE_OFFER" <<'EOF'
Freezeit is distributed under GPL-3.0-or-later.
https://github.com/szh1118/freezeit_cos16/tree/0000000000000000000000000000000000000000
EOF
(
  cd "$tmp"
  find . -mindepth 1 -maxdepth 1 -type f ! -name SHA256SUMS -printf '%P\0' \
    | LC_ALL=C sort -z | xargs -0 sha256sum >SHA256SUMS
  bsdtar --format zip -cf "$tmp/release.zip" $(find . -mindepth 1 -maxdepth 1 ! -name release.zip -printf '%P\n' | LC_ALL=C sort)
)
"$ROOT/scripts/validate-release-zip.sh" "$tmp/release.zip" >/dev/null
if no_session_pin_output="$(env -u FREEZEIT_EXPECTED_BUILD_SESSION_MANIFEST_SHA256 "$ROOT/scripts/validate-release-zip.sh" "$tmp/release.zip" 2>&1)"; then
  fail "validator accepted a released ZIP without a trusted build-session manifest digest"
fi
grep -F 'FREEZEIT_EXPECTED_BUILD_SESSION_MANIFEST_SHA256' <<<"$no_session_pin_output" >/dev/null \
  || fail "missing trusted build-session digest was rejected for the wrong reason"
if FREEZEIT_EXPECTED_APK_SIGNER_SHA256=cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc \
  "$ROOT/scripts/validate-release-zip.sh" "$tmp/release.zip" >/dev/null 2>&1; then
  fail "validator accepted mismatched expected signer provenance"
fi

forged_manifest_backup="${tmp}-forged-manifest"
forged_provenance_backup="${tmp}-forged-provenance"
forged_sums_backup="${tmp}-forged-sums"
cp "$tmp/build-session.manifest" "$forged_manifest_backup"
cp "$tmp/provenance.txt" "$forged_provenance_backup"
cp "$tmp/SHA256SUMS" "$forged_sums_backup"
forged_session_id=fedcba9876543210fedcba9876543210
sed -i "s/^sessionId=.*/sessionId=$forged_session_id/" "$tmp/build-session.manifest"
forged_session_sha="$(sha256sum "$tmp/build-session.manifest" | awk '{print $1}')"
sed -i "s/^buildSessionId=.*/buildSessionId=$forged_session_id/" "$tmp/provenance.txt"
sed -i "s/^buildSessionManifestSha256=.*/buildSessionManifestSha256=$forged_session_sha/" "$tmp/provenance.txt"
(
  cd "$tmp"
  find . -mindepth 1 -maxdepth 1 -type f ! -name SHA256SUMS ! -name '*.zip' -printf '%P\0' \
    | LC_ALL=C sort -z | xargs -0 sha256sum >SHA256SUMS
  bsdtar --format zip -cf "$tmp/forged-complete-session.zip" $(find . -mindepth 1 -maxdepth 1 -type f ! -name '*.zip' -printf '%P\n' | LC_ALL=C sort)
)
cp "$forged_manifest_backup" "$tmp/build-session.manifest"
cp "$forged_provenance_backup" "$tmp/provenance.txt"
cp "$forged_sums_backup" "$tmp/SHA256SUMS"
rm -f "$forged_manifest_backup" "$forged_provenance_backup" "$forged_sums_backup"
if forged_session_output="$(FREEZEIT_EXPECTED_BUILD_SESSION_MANIFEST_SHA256="$trusted_session_sha" "$ROOT/scripts/validate-release-zip.sh" "$tmp/forged-complete-session.zip" 2>&1)"; then
  fail "validator accepted a self-consistent forged build-session manifest"
fi
grep -F 'trusted build session manifest digest' <<<"$forged_session_output" >/dev/null \
  || fail "forged build-session manifest was rejected for the wrong reason"

static_daemon_backup="${tmp}-static-daemon"
static_manifest_backup="${tmp}-static-manifest"
static_provenance_backup="${tmp}-static-provenance"
static_sums_backup="${tmp}-static-sums"
cp "$tmp/freezeit" "$static_daemon_backup"
cp "$tmp/build-session.manifest" "$static_manifest_backup"
cp "$tmp/provenance.txt" "$static_provenance_backup"
cp "$tmp/SHA256SUMS" "$static_sums_backup"
make_static_aarch64_elf "$tmp/freezeit"
static_daemon_sha="$(sha256sum "$tmp/freezeit" | awk '{print $1}')"
sed -i "s/^daemonSha256=.*/daemonSha256=$static_daemon_sha/" "$tmp/build-session.manifest" "$tmp/provenance.txt"
static_session_sha="$(sha256sum "$tmp/build-session.manifest" | awk '{print $1}')"
sed -i "s/^buildSessionManifestSha256=.*/buildSessionManifestSha256=$static_session_sha/" "$tmp/provenance.txt"
(
  cd "$tmp"
  find . -mindepth 1 -maxdepth 1 -type f ! -name SHA256SUMS ! -name '*.zip' -printf '%P\0' \
    | LC_ALL=C sort -z | xargs -0 sha256sum >SHA256SUMS
  bsdtar --format zip -cf "$tmp/static-android.zip" $(find . -mindepth 1 -maxdepth 1 -type f ! -name '*.zip' -printf '%P\n' | LC_ALL=C sort)
)
cp "$static_daemon_backup" "$tmp/freezeit"
cp "$static_manifest_backup" "$tmp/build-session.manifest"
cp "$static_provenance_backup" "$tmp/provenance.txt"
cp "$static_sums_backup" "$tmp/SHA256SUMS"
rm -f "$static_daemon_backup" "$static_manifest_backup" "$static_provenance_backup" "$static_sums_backup"
if static_output="$(FREEZEIT_EXPECTED_BUILD_SESSION_MANIFEST_SHA256="$static_session_sha" "$ROOT/scripts/validate-release-zip.sh" "$tmp/static-android.zip" 2>&1)"; then
  :
else
  fail "validator rejected a static Android AArch64 executable: $static_output"
fi

android_elf_backup="${tmp}-android-freezeit"
cp "$tmp/freezeit" "$android_elf_backup"
make_static_aarch64_elf "$tmp/freezeit"
printf '\003\000' | dd of="$tmp/freezeit" bs=1 seek=16 conv=notrunc status=none
(
  cd "$tmp"
  bsdtar --format zip -cf "$tmp/shared-object.zip" $(find . -mindepth 1 -maxdepth 1 -type f ! -name '*.zip' -printf '%P\n' | LC_ALL=C sort)
)
cp "$android_elf_backup" "$tmp/freezeit"
if shared_object_output="$("$ROOT/scripts/validate-release-zip.sh" "$tmp/shared-object.zip" 2>&1)"; then
  fail "validator accepted an ET_DYN daemon without an Android PT_INTERP"
fi
grep -F 'must have exactly one Android PT_INTERP' <<<"$shared_object_output" >/dev/null \
  || fail "ET_DYN without PT_INTERP was rejected for the wrong reason"

make_android_aarch64_elf "$tmp/freezeit"
python3 - "$tmp/freezeit" <<'PY'
import struct
import sys

path = sys.argv[1]
with open(path, "rb") as stream:
    data = bytearray(stream.read())
program_offset = struct.unpack_from("<Q", data, 32)[0]
program_size = struct.unpack_from("<H", data, 54)[0]
program_count = struct.unpack_from("<H", data, 56)[0]
for index in range(program_count):
    offset = program_offset + index * program_size
    if struct.unpack_from("<I", data, offset)[0] == 2:  # PT_DYNAMIC
        struct.pack_into("<I", data, offset, 4)  # PT_NOTE
        break
else:
    raise SystemExit("fixture has no PT_DYNAMIC")
with open(path, "wb") as stream:
    stream.write(data)
PY
(
  cd "$tmp"
  bsdtar --format zip -cf "$tmp/dynamic-without-metadata.zip" $(find . -mindepth 1 -maxdepth 1 -type f ! -name '*.zip' -printf '%P\n' | LC_ALL=C sort)
)
cp "$android_elf_backup" "$tmp/freezeit"
if missing_dynamic_output="$("$ROOT/scripts/validate-release-zip.sh" "$tmp/dynamic-without-metadata.zip" 2>&1)"; then
  fail "validator accepted an ET_DYN daemon without PT_DYNAMIC"
fi
grep -F 'must have exactly one PT_DYNAMIC' <<<"$missing_dynamic_output" >/dev/null \
  || fail "ET_DYN without PT_DYNAMIC was rejected for the wrong reason"

make_static_aarch64_elf "$tmp/freezeit"
printf '\004\000\000\000' | dd of="$tmp/freezeit" bs=1 seek=68 conv=notrunc status=none
(
  cd "$tmp"
  bsdtar --format zip -cf "$tmp/non-executable-load.zip" $(find . -mindepth 1 -maxdepth 1 -type f ! -name '*.zip' -printf '%P\n' | LC_ALL=C sort)
)
cp "$android_elf_backup" "$tmp/freezeit"
rm -f "$android_elf_backup"
if non_executable_load_output="$("$ROOT/scripts/validate-release-zip.sh" "$tmp/non-executable-load.zip" 2>&1)"; then
  fail "validator accepted a daemon with no executable PT_LOAD segment"
fi
grep -F 'has no executable PT_LOAD segment' <<<"$non_executable_load_output" >/dev/null \
  || fail "non-executable PT_LOAD daemon was rejected for the wrong reason"

source_offer_backup="${tmp}-source-offer"
source_sums_backup="${tmp}-source-sums"
cp "$tmp/SOURCE_OFFER" "$source_offer_backup"
cp "$tmp/SHA256SUMS" "$source_sums_backup"
custom_source_repository_url='https://git.example/freezeit'
cat >"$tmp/SOURCE_OFFER" <<EOF
Freezeit is distributed under GPL-3.0-or-later.
Corresponding source for this package is available at:
$custom_source_repository_url/tree/0000000000000000000000000000000000000000

Source commit: 0000000000000000000000000000000000000000
Daemon source: freezeitDaemon/
Android manager source: freezeitApp/
EOF
(
  cd "$tmp"
  find . -mindepth 1 -maxdepth 1 -type f ! -name SHA256SUMS ! -name '*.zip' -printf '%P\0' \
    | LC_ALL=C sort -z | xargs -0 sha256sum >SHA256SUMS
  bsdtar --format zip -cf "$tmp/custom-source-url.zip" $(find . -mindepth 1 -maxdepth 1 -type f ! -name '*.zip' -printf '%P\n' | LC_ALL=C sort)
)
SOURCE_REPOSITORY_URL="$custom_source_repository_url" \
  "$ROOT/scripts/validate-release-zip.sh" "$tmp/custom-source-url.zip" >/dev/null
cp "$source_offer_backup" "$tmp/SOURCE_OFFER"
cp "$source_sums_backup" "$tmp/SHA256SUMS"
rm -f "$source_offer_backup" "$source_sums_backup"

if size_limit_output="$(FREEZEIT_MAX_RELEASE_ENTRY_BYTES=1 "$ROOT/scripts/validate-release-zip.sh" "$tmp/release.zip" 2>&1)"; then
  fail "validator accepted an archive above the configured extraction size limit"
fi
grep -F 'entry exceeds uncompressed size limit' <<<"$size_limit_output" >/dev/null \
  || fail "archive size-limit rejection did not happen during central-directory inspection"

session_manifest_backup="${tmp}-build-session.manifest"
mv "$tmp/build-session.manifest" "$session_manifest_backup"
(
  cd "$tmp"
  find . -mindepth 1 -maxdepth 1 -type f ! -name SHA256SUMS ! -name '*.zip' -printf '%P\0' \
    | LC_ALL=C sort -z | xargs -0 sha256sum >SHA256SUMS
  bsdtar --format zip -cf "$tmp/session-provenance-only.zip" $(find . -mindepth 1 -maxdepth 1 -type f ! -name '*.zip' -printf '%P\n' | LC_ALL=C sort)
)
mv "$session_manifest_backup" "$tmp/build-session.manifest"
if session_output="$("$ROOT/scripts/validate-release-zip.sh" "$tmp/session-provenance-only.zip" 2>&1)"; then
  fail "validator accepted a released ZIP with only self-reported session provenance"
fi
grep -F 'archive is missing its build session manifest' <<<"$session_output" >/dev/null \
  || fail "missing build-session manifest was rejected for the wrong reason"

android_elf_backup="${tmp}-android-freezeit"
cp "$tmp/freezeit" "$android_elf_backup"
cp /bin/true "$tmp/freezeit"
printf '\267\000' | dd of="$tmp/freezeit" bs=1 seek=18 conv=notrunc status=none
(
  cd "$tmp"
  bsdtar --format zip -cf "$tmp/linux-interpreter.zip" $(find . -mindepth 1 -maxdepth 1 -type f ! -name '*.zip' -printf '%P\n' | LC_ALL=C sort)
)
cp "$android_elf_backup" "$tmp/freezeit"
if linux_interpreter_output="$("$ROOT/scripts/validate-release-zip.sh" "$tmp/linux-interpreter.zip" 2>&1)"; then
  fail "validator accepted an AArch64 ELF linked to the Linux dynamic linker"
fi
grep -F 'not linked for the Android AArch64 dynamic linker' <<<"$linux_interpreter_output" >/dev/null \
  || fail "Linux dynamic-linker ELF was rejected for the wrong reason"

python3 - "$tmp/freezeit" <<'PY'
import struct
import sys

header = bytearray(64)
header[:16] = b"\x7fELF\x02\x01\x01\x00" + b"\x00" * 8
struct.pack_into("<HHI", header, 16, 1, 183, 1)  # ET_REL, EM_AARCH64, EV_CURRENT.
struct.pack_into("<HHH", header, 52, 64, 0, 0)
with open(sys.argv[1], "wb") as stream:
    stream.write(header)
PY
(
  cd "$tmp"
  bsdtar --format zip -cf "$tmp/rel-object.zip" $(find . -mindepth 1 -maxdepth 1 -type f ! -name '*.zip' -printf '%P\n' | LC_ALL=C sort)
)
cp "$android_elf_backup" "$tmp/freezeit"
rm -f "$android_elf_backup"
if rel_object_output="$("$ROOT/scripts/validate-release-zip.sh" "$tmp/rel-object.zip" 2>&1)"; then
  fail "validator accepted an AArch64 relocatable object as the daemon"
fi
grep -F 'freezeit is not an executable ELF' <<<"$rel_object_output" >/dev/null \
  || fail "AArch64 relocatable object was rejected for the wrong reason"

sed -i 's/^apkSignerSha256=.*/apkSignerSha256=cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc/' "$tmp/provenance.txt"
(
  cd "$tmp"
  find . -mindepth 1 -maxdepth 1 -type f ! -name SHA256SUMS ! -name '*.zip' -printf '%P\0' \
    | LC_ALL=C sort -z | xargs -0 sha256sum >SHA256SUMS
  bsdtar --format zip -cf "$tmp/signer-provenance-only.zip" $(find . -mindepth 1 -maxdepth 1 -type f ! -name '*.zip' -printf '%P\n' | LC_ALL=C sort)
)
if FREEZEIT_EXPECTED_APK_SIGNER_SHA256=cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc \
  "$ROOT/scripts/validate-release-zip.sh" "$tmp/signer-provenance-only.zip" >/dev/null 2>&1; then
  fail "validator trusted a substituted APK signer provenance value"
fi
sed -i 's/^apkSignerSha256=.*/apkSignerSha256=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb/' "$tmp/provenance.txt"

cp "$tmp/freezeit" "$tmp/freezeitARM64"
(
  cd "$tmp"
  bsdtar --format zip -cf "$tmp/legacy.zip" $(find . -mindepth 1 -maxdepth 1 ! -name release.zip ! -name legacy.zip -printf '%P\n' | LC_ALL=C sort)
)
if "$ROOT/scripts/validate-release-zip.sh" "$tmp/legacy.zip" >/dev/null 2>&1; then
  fail "validator accepted a forbidden legacy daemon"
fi
rm -f "$tmp/freezeitARM64"

cp "$tmp/freezeit.apk" "$tmp/second.apk"
(
  cd "$tmp"
  find . -mindepth 1 -maxdepth 1 -type f ! -name SHA256SUMS ! -name '*.zip' -printf '%P\0' \
    | LC_ALL=C sort -z | xargs -0 sha256sum >SHA256SUMS
  bsdtar --format zip -cf "$tmp/two-apk.zip" $(find . -mindepth 1 -maxdepth 1 -type f ! -name '*.zip' -printf '%P\n' | LC_ALL=C sort)
)
if "$ROOT/scripts/validate-release-zip.sh" "$tmp/two-apk.zip" >/dev/null 2>&1; then
  fail "validator accepted a second APK"
fi
rm -f "$tmp/second.apk"

cp "$tmp/freezeit" "$tmp/second-daemon"
(
  cd "$tmp"
  find . -mindepth 1 -maxdepth 1 -type f ! -name SHA256SUMS ! -name '*.zip' -printf '%P\0' \
    | LC_ALL=C sort -z | xargs -0 sha256sum >SHA256SUMS
  bsdtar --format zip -cf "$tmp/two-elf.zip" $(find . -mindepth 1 -maxdepth 1 -type f ! -name '*.zip' -printf '%P\n' | LC_ALL=C sort)
)
if "$ROOT/scripts/validate-release-zip.sh" "$tmp/two-elf.zip" >/dev/null 2>&1; then
  fail "validator accepted a second ELF daemon"
fi
rm -f "$tmp/second-daemon"

python3 - "$tmp/traversal.zip" <<'PY'
import zipfile, sys
with zipfile.ZipFile(sys.argv[1], "w") as archive:
    archive.writestr("../escape", "bad")
PY
if "$ROOT/scripts/validate-release-zip.sh" "$tmp/traversal.zip" >/dev/null 2>&1; then
  fail "validator accepted zip-slip path"
fi

python3 - "$tmp/duplicate.zip" <<'PY'
import warnings, zipfile, sys
warnings.filterwarnings("ignore", message="Duplicate name:.*")
with zipfile.ZipFile(sys.argv[1], "w") as archive:
    archive.writestr("module.prop", "one")
    archive.writestr("module.prop", "two")
PY
if "$ROOT/scripts/validate-release-zip.sh" "$tmp/duplicate.zip" >/dev/null 2>&1; then
  fail "validator accepted duplicate paths"
fi

python3 - "$tmp/symlink.zip" <<'PY'
import stat, zipfile, sys
info = zipfile.ZipInfo("freezeit")
info.create_system = 3
info.external_attr = (stat.S_IFLNK | 0o777) << 16
with zipfile.ZipFile(sys.argv[1], "w") as archive:
    archive.writestr(info, "/system/bin/sh")
PY
if "$ROOT/scripts/validate-release-zip.sh" "$tmp/symlink.zip" >/dev/null 2>&1; then
  fail "validator accepted symlink entry"
fi

cp "$tmp/freezeit.apk" "$tmp/unexpected.bin"
(
  cd "$tmp"
  find . -mindepth 1 -maxdepth 1 -type f ! -name SHA256SUMS ! -name '*.zip' -printf '%P\0' \
    | LC_ALL=C sort -z | xargs -0 sha256sum >SHA256SUMS
  bsdtar --format zip -cf "$tmp/unexpected.zip" $(find . -mindepth 1 -maxdepth 1 -type f ! -name '*.zip' -printf '%P\n' | LC_ALL=C sort)
)
if unexpected_output="$(FREEZEIT_EXPECTED_BUILD_SESSION_MANIFEST_SHA256="$trusted_session_sha" \
  "$ROOT/scripts/validate-release-zip.sh" "$tmp/unexpected.zip" 2>&1)"; then
  fail "validator accepted non-allowlisted entry"
fi
grep -F 'entry is not allowlisted: unexpected.bin' <<<"$unexpected_output" >/dev/null \
  || fail "unexpected.bin was rejected before the allowlist check"
rm -f "$tmp/unexpected.bin"

sed -i 's/^daemonSha256=.*/daemonSha256=0000000000000000000000000000000000000000000000000000000000000000/' "$tmp/provenance.txt"
(
  cd "$tmp"
  find . -mindepth 1 -maxdepth 1 -type f ! -name SHA256SUMS ! -name '*.zip' -printf '%P\0' | LC_ALL=C sort -z | xargs -0 sha256sum >SHA256SUMS
  bsdtar --format zip -cf "$tmp/bad-provenance.zip" $(find . -mindepth 1 -maxdepth 1 -type f ! -name '*.zip' -printf '%P\n' | LC_ALL=C sort)
)
if "$ROOT/scripts/validate-release-zip.sh" "$tmp/bad-provenance.zip" >/dev/null 2>&1; then
  fail "validator accepted mismatched provenance digest"
fi
sed -i "s/^daemonSha256=.*/daemonSha256=$daemon_sha/" "$tmp/provenance.txt"
sed -i 's/^apkSignerSha256=.*/apkSignerSha256=unverified/' "$tmp/provenance.txt"
(
  cd "$tmp"
  find . -mindepth 1 -maxdepth 1 -type f ! -name SHA256SUMS ! -name '*.zip' -printf '%P\0' | LC_ALL=C sort -z | xargs -0 sha256sum >SHA256SUMS
  bsdtar --format zip -cf "$tmp/unverified-release-signer.zip" $(find . -mindepth 1 -maxdepth 1 -type f ! -name '*.zip' -printf '%P\n' | LC_ALL=C sort)
)
if "$ROOT/scripts/validate-release-zip.sh" "$tmp/unverified-release-signer.zip" >/dev/null 2>&1; then
  fail "validator accepted an unverified released APK signer"
fi
sed -i 's/^apkSignerSha256=.*/apkSignerSha256=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb/' "$tmp/provenance.txt"

cat >"$tmp/released-update.json" <<EOF
{
  "version": "$expected_version",
  "versionCode": $expected_code,
  "zipUrl": "https://example.invalid/freezeit_oneplus13_android16_selfuse_v${expected_version}_${expected_code}.zip",
  "zipSha256": "PLACEHOLDER",
  "changelog": "https://example.invalid/changelog.txt"
}
EOF
cp "$tmp/release.zip" "$tmp/freezeit_oneplus13_android16_selfuse_v${expected_version}_${expected_code}.zip"
release_sha="$(sha256sum "$tmp/freezeit_oneplus13_android16_selfuse_v${expected_version}_${expected_code}.zip" | awk '{print $1}')"
sed -i "s/PLACEHOLDER/$release_sha/" "$tmp/released-update.json"
mkdir -p "$metadata_root/magisk" "$metadata_root/scripts" "$metadata_root/freezeitRelease"
cp "$ROOT/magisk/module.prop" "$metadata_root/magisk/module.prop"
cp "$ROOT/scripts/test-release-metadata.sh" "$metadata_root/scripts/test-release-metadata.sh"
cp "$ROOT/scripts/validate-release-zip.sh" "$metadata_root/scripts/validate-release-zip.sh"
printf '`%s` `%s` GPL-3.0-or-later\n' "$expected_version" "$expected_code" >"$metadata_root/README.md"
printf '`%s` `%s` GPL-3.0-or-later\n' "$expected_version" "$expected_code" >"$metadata_root/freezeitRelease/README.md"
UPDATE_JSON="$tmp/released-update.json" RELEASE_DIR="$tmp" \
  FREEZEIT_EXPECTED_BUILD_SESSION_MANIFEST_SHA256="$trusted_session_sha" \
  "$metadata_root/scripts/test-release-metadata.sh" released "$expected_version" "$expected_code" >/dev/null
sed -i "s/^version=.*/version=${expected_version}-dev/" "$metadata_root/magisk/module.prop"
if UPDATE_JSON="$tmp/released-update.json" RELEASE_DIR="$tmp" \
  FREEZEIT_EXPECTED_BUILD_SESSION_MANIFEST_SHA256="$trusted_session_sha" \
  "$metadata_root/scripts/test-release-metadata.sh" released "$expected_version" "$expected_code" >/dev/null 2>&1; then
  fail "metadata test accepted a version-prefix module.prop value"
fi
sed -i "s/^version=.*/version=$expected_version/" "$metadata_root/magisk/module.prop"
sed -i "s/^versionCode=.*/versionCode=${expected_code}0/" "$metadata_root/magisk/module.prop"
if UPDATE_JSON="$tmp/released-update.json" RELEASE_DIR="$tmp" \
  FREEZEIT_EXPECTED_BUILD_SESSION_MANIFEST_SHA256="$trusted_session_sha" \
  "$metadata_root/scripts/test-release-metadata.sh" released "$expected_version" "$expected_code" >/dev/null 2>&1; then
  fail "metadata test accepted a versionCode-prefix module.prop value"
fi
sed -i "s/^versionCode=.*/versionCode=$expected_code/" "$metadata_root/magisk/module.prop"

# Exercise release-metadata provenance checks independently from the archive
# validator, which has its own fixture coverage above.
cat >"$metadata_root/scripts/validate-release-zip.sh" <<'EOF'
#!/usr/bin/env sh
exit 0
EOF
chmod 0755 "$metadata_root/scripts/validate-release-zip.sh"
provenance_backup="${tmp}-metadata-provenance"
cp "$tmp/provenance.txt" "$provenance_backup"

sed -i 's/^releaseKind=.*/releaseKind=candidate/' "$tmp/provenance.txt"
(
  cd "$tmp"
  find . -mindepth 1 -maxdepth 1 -type f ! -name SHA256SUMS ! -name '*.zip' -printf '%P\0' \
    | LC_ALL=C sort -z | xargs -0 sha256sum >SHA256SUMS
  bsdtar --format zip -cf "$tmp/candidate-provenance.zip" $(find . -mindepth 1 -maxdepth 1 -type f ! -name '*.zip' -printf '%P\n' | LC_ALL=C sort)
)
candidate_sha="$(sha256sum "$tmp/candidate-provenance.zip" | awk '{print $1}')"
cp "$tmp/candidate-provenance.zip" "$tmp/freezeit_oneplus13_android16_selfuse_v${expected_version}_${expected_code}.zip"
sed -i "s/$release_sha/$candidate_sha/" "$tmp/released-update.json"
if UPDATE_JSON="$tmp/released-update.json" RELEASE_DIR="$tmp" \
  FREEZEIT_EXPECTED_BUILD_SESSION_MANIFEST_SHA256="$trusted_session_sha" \
  "$metadata_root/scripts/test-release-metadata.sh" released "$expected_version" "$expected_code" >/dev/null 2>&1; then
  fail "metadata test accepted candidate provenance for a released update"
fi

cp "$provenance_backup" "$tmp/provenance.txt"
sed -i 's/^dirty=.*/dirty=true/' "$tmp/provenance.txt"
(
  cd "$tmp"
  find . -mindepth 1 -maxdepth 1 -type f ! -name SHA256SUMS ! -name '*.zip' -printf '%P\0' \
    | LC_ALL=C sort -z | xargs -0 sha256sum >SHA256SUMS
  bsdtar --format zip -cf "$tmp/dirty-provenance.zip" $(find . -mindepth 1 -maxdepth 1 -type f ! -name '*.zip' -printf '%P\n' | LC_ALL=C sort)
)
dirty_sha="$(sha256sum "$tmp/dirty-provenance.zip" | awk '{print $1}')"
cp "$tmp/dirty-provenance.zip" "$tmp/freezeit_oneplus13_android16_selfuse_v${expected_version}_${expected_code}.zip"
sed -i "s/$candidate_sha/$dirty_sha/" "$tmp/released-update.json"
if UPDATE_JSON="$tmp/released-update.json" RELEASE_DIR="$tmp" \
  FREEZEIT_EXPECTED_BUILD_SESSION_MANIFEST_SHA256="$trusted_session_sha" \
  "$metadata_root/scripts/test-release-metadata.sh" released "$expected_version" "$expected_code" >/dev/null 2>&1; then
  fail "metadata test accepted dirty provenance for a released update"
fi
rm -f "$provenance_backup"

cat >"$tmp/planned-version-only.json" <<EOF
{
  "version": "$expected_version",
  "versionCode": $((expected_code - 1)),
  "zipUrl": "https://example.invalid/old.zip",
  "zipSha256": "",
  "changelog": "https://example.invalid/changelog.txt"
}
EOF
if UPDATE_JSON="$tmp/planned-version-only.json" "$ROOT/scripts/test-release-metadata.sh" planned >/dev/null 2>&1; then
  fail "metadata test accepted an early planned version"
fi
cat >"$tmp/planned-code-only.json" <<EOF
{
  "version": "${expected_version}-previous",
  "versionCode": $expected_code,
  "zipUrl": "https://example.invalid/old.zip",
  "zipSha256": "",
  "changelog": "https://example.invalid/changelog.txt"
}
EOF
if UPDATE_JSON="$tmp/planned-code-only.json" "$ROOT/scripts/test-release-metadata.sh" planned >/dev/null 2>&1; then
  fail "metadata test accepted an early planned versionCode"
fi
if "$ROOT/scripts/test-release-metadata.sh" planned '3.3.0;touch-pwned' 303000 >/dev/null 2>&1; then
  fail "metadata test accepted unsafe version input"
fi
printf '{broken json\n' >"$tmp/broken-update.json"
if UPDATE_JSON="$tmp/broken-update.json" "$ROOT/scripts/test-release-metadata.sh" planned >/dev/null 2>&1; then
  fail "metadata test accepted malformed JSON"
fi
if [[ -n "$original_apksigner" ]]; then
  export APKSIGNER="$original_apksigner"
else
  unset APKSIGNER
fi
if [[ -n "$original_expected_apk_signer_sha" ]]; then
  export FREEZEIT_EXPECTED_APK_SIGNER_SHA256="$original_expected_apk_signer_sha"
else
  unset FREEZEIT_EXPECTED_APK_SIGNER_SHA256
fi
if [[ -n "$original_expected_session_manifest_sha" ]]; then
  export FREEZEIT_EXPECTED_BUILD_SESSION_MANIFEST_SHA256="$original_expected_session_manifest_sha"
else
  unset FREEZEIT_EXPECTED_BUILD_SESSION_MANIFEST_SHA256
fi
if [[ -n "$(git -C "$ROOT" status --short -- freezeitRelease/update.json)" ]]; then
  "$ROOT/scripts/test-release-metadata.sh" released "$expected_version" "$expected_code" >/dev/null \
    || fail "changed update.json does not describe a validated release"
fi
echo "release pipeline structure: pass"
