#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fail() { echo "release pipeline test failed: $*" >&2; exit 1; }

[[ -d "$ROOT/magisk" ]] || fail "top-level magisk template missing"
for forbidden in freezeit freezeitARM64 freezeitX64 freezeitRustARM64 freezeitRustX64; do
  [[ ! -e "$ROOT/magisk/$forbidden" ]] || fail "template contains daemon: $forbidden"
done
if find "$ROOT/magisk" -maxdepth 1 -type f -name '*.apk' -print -quit | grep -q .; then
  fail "template contains APK"
fi
grep -Fx 'version=3.3.0SelfUse' "$ROOT/magisk/module.prop" >/dev/null || fail "planned version missing"
grep -Fx 'versionCode=303000' "$ROOT/magisk/module.prop" >/dev/null || fail "planned versionCode missing"
grep -F '仅支持 ARM64' "$ROOT/magisk/customize.sh" >/dev/null || fail "installer does not reject non-ARM64"
grep -F 'freezeitARM64 freezeitX64' "$ROOT/magisk/customize.sh" >/dev/null || fail "legacy daemon rejection missing"
grep -F 'freezeitVS/magisk' "$ROOT/scripts/package-release.sh" >/dev/null && fail "packager still uses legacy template"
grep -F 'freezeitARM64' "$ROOT/scripts/package-release.sh" >/dev/null || fail "packager does not reject legacy daemon names"
grep -F 'working tree is dirty' "$ROOT/scripts/package-release.sh" >/dev/null || fail "packager does not reject dirty trees by default"
grep -F 'source-snapshot.tar.gz' "$ROOT/scripts/package-release.sh" >/dev/null || fail "dirty candidate source snapshot is missing"
grep -F '[[ -e "$ROOT/$path" || -L "$ROOT/$path" ]]' "$ROOT/scripts/package-release.sh" >/dev/null || fail "dirty candidate snapshot does not skip deleted paths"
grep -F 'sourcePatchSha256' "$ROOT/scripts/package-release.sh" >/dev/null || fail "dirty candidate patch digest is missing"
if "$ROOT/scripts/package-release.sh" >/dev/null 2>&1; then
  fail "default packager accepted the dirty review tree"
fi

if "$ROOT/scripts/validate-release-zip.sh" /nonexistent/freezeit.zip >/dev/null 2>&1; then
  fail "validator accepted a missing zip"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cp -a "$ROOT/magisk/." "$tmp/"
cp /bin/true "$tmp/freezeit"
printf '\267\000' | dd of="$tmp/freezeit" bs=1 seek=18 conv=notrunc status=none
printf 'test apk\n' >"$tmp/freezeit.apk"
cp "$ROOT/LICENSE" "$tmp/LICENSE"
daemon_sha="$(sha256sum "$tmp/freezeit" | awk '{print $1}')"
apk_sha="$(sha256sum "$tmp/freezeit.apk" | awk '{print $1}')"
cat >"$tmp/provenance.txt" <<EOF
format=freezeit-release-provenance-v1
version=3.3.0SelfUse
versionCode=303000
gitCommit=0000000000000000000000000000000000000000
releaseKind=released
dirty=false
daemonSource=freezeitDaemon
managerSource=freezeitApp
daemonTarget=aarch64-linux-android
daemonSha256=$daemon_sha
apkSha256=$apk_sha
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

cp "$tmp/freezeit" "$tmp/freezeitARM64"
(
  cd "$tmp"
  bsdtar --format zip -cf "$tmp/legacy.zip" $(find . -mindepth 1 -maxdepth 1 ! -name release.zip ! -name legacy.zip -printf '%P\n' | LC_ALL=C sort)
)
if "$ROOT/scripts/validate-release-zip.sh" "$tmp/legacy.zip" >/dev/null 2>&1; then
  fail "validator accepted a forbidden legacy daemon"
fi

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
  bsdtar --format zip -cf "$tmp/unexpected.zip" module.prop customize.sh service.sh uninstall.sh rom_baseline.prop changelog.txt freezeit freezeit.apk LICENSE SOURCE_OFFER SHA256SUMS provenance.txt unexpected.bin
)
if "$ROOT/scripts/validate-release-zip.sh" "$tmp/unexpected.zip" >/dev/null 2>&1; then
  fail "validator accepted non-allowlisted entry"
fi
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

status_before="$(git -C "$ROOT" status --short -- freezeitRelease/update.json)"
[[ -z "$status_before" ]] || fail "update.json changed before a validated 3.3.0 release exists"

cat >"$tmp/released-update.json" <<'EOF'
{
  "version": "3.3.0SelfUse",
  "versionCode": 303000,
  "zipUrl": "https://example.invalid/freezeit_oneplus13_android16_selfuse_v3.3.0SelfUse_303000.zip",
  "zipSha256": "PLACEHOLDER",
  "changelog": "https://example.invalid/changelog.txt"
}
EOF
cp "$tmp/release.zip" "$tmp/freezeit_oneplus13_android16_selfuse_v3.3.0SelfUse_303000.zip"
release_sha="$(sha256sum "$tmp/freezeit_oneplus13_android16_selfuse_v3.3.0SelfUse_303000.zip" | awk '{print $1}')"
sed -i "s/PLACEHOLDER/$release_sha/" "$tmp/released-update.json"
UPDATE_JSON="$tmp/released-update.json" RELEASE_DIR="$tmp" "$ROOT/scripts/test-release-metadata.sh" released >/dev/null
if "$ROOT/scripts/test-release-metadata.sh" planned '3.3.0;touch-pwned' 303000 >/dev/null 2>&1; then
  fail "metadata test accepted unsafe version input"
fi
printf '{broken json\n' >"$tmp/broken-update.json"
if UPDATE_JSON="$tmp/broken-update.json" "$ROOT/scripts/test-release-metadata.sh" planned >/dev/null 2>&1; then
  fail "metadata test accepted malformed JSON"
fi
echo "release pipeline structure: pass"
