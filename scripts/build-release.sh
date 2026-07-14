#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EXPECTED_VERSION="${EXPECTED_VERSION:-3.3.3SelfUse}"
EXPECTED_VERSION_CODE="${EXPECTED_VERSION_CODE:-303003}"
RELEASE_KIND="${RELEASE_KIND:-released}"
ALLOW_DIRTY="${ALLOW_DIRTY:-0}"
BUILD_SESSION_ROOT="${BUILD_SESSION_ROOT:-$ROOT/.release-staging}"
DAEMON="$ROOT/freezeitDaemon/target/aarch64-linux-android/release/freezeit"
APK_OUTPUT_DIR="$ROOT/freezeitApp/app/build/outputs/apk/release"

fail() { echo "release build failed: $*" >&2; exit 1; }
[[ "$RELEASE_KIND" == released || "$RELEASE_KIND" == candidate ]] \
  || fail "RELEASE_KIND must be released or candidate"
if [[ "$RELEASE_KIND" == released ]]; then
  [[ -n "${FREEZEIT_EXPECTED_APK_SIGNER_SHA256:-}" ]] \
    || fail "RELEASE_KIND=released requires FREEZEIT_EXPECTED_APK_SIGNER_SHA256"
  normalized_signer="$(printf '%s' "$FREEZEIT_EXPECTED_APK_SIGNER_SHA256" | tr -d '[:space:]:' | tr '[:upper:]' '[:lower:]')"
  [[ "$normalized_signer" =~ ^[0-9a-f]{64}$ ]] \
    || fail "FREEZEIT_EXPECTED_APK_SIGNER_SHA256 must be a SHA-256 certificate digest"
  [[ -z "$(git -C "$ROOT" status --porcelain=v1 --untracked-files=all)" ]] \
    || fail "RELEASE_KIND=released requires a clean working tree"
fi

start_commit="$(git -C "$ROOT" rev-parse HEAD)"
rm -f "$DAEMON"
rm -rf "$APK_OUTPUT_DIR"
export RELEASE_KIND
"$ROOT/freezeitDaemon/scripts/build-android.sh"
(
  cd "$ROOT/freezeitApp"
  bash ./gradlew :app:assembleRelease --no-daemon
)

[[ -f "$DAEMON" && ! -L "$DAEMON" ]] || fail "daemon build did not produce a regular file"
mapfile -t apks < <(find "$APK_OUTPUT_DIR" -maxdepth 1 -type f -name '*.apk' -print | LC_ALL=C sort)
[[ ${#apks[@]} -eq 1 ]] || { echo "expected exactly one release APK; found ${#apks[@]}" >&2; exit 1; }
APK="${apks[0]}"
APK_METADATA="$APK_OUTPUT_DIR/output-metadata.json"
[[ -f "$APK_METADATA" && ! -L "$APK_METADATA" ]] || fail "APK metadata is missing or unsafe"
end_commit="$(git -C "$ROOT" rev-parse HEAD)"
[[ "$end_commit" == "$start_commit" ]] || fail "HEAD changed while release artifacts were building"
if [[ "$RELEASE_KIND" == released ]]; then
  [[ -z "$(git -C "$ROOT" status --porcelain=v1 --untracked-files=all)" ]] \
    || fail "working tree changed while released artifacts were building"
fi

mkdir -p "$BUILD_SESSION_ROOT"
chmod 0700 "$BUILD_SESSION_ROOT"
session_dir="$(mktemp -d "$BUILD_SESSION_ROOT/build-session.XXXXXX")"
trap 'rm -rf "$session_dir"' EXIT
session_id="$(python3 - <<'PY'
import secrets
print(secrets.token_hex(16))
PY
)"
session_file="$session_dir/session.manifest"
daemon_path="$(realpath -e "$DAEMON")"
apk_path="$(realpath -e "$APK")"
metadata_path="$(realpath -e "$APK_METADATA")"
daemon_sha="$(sha256sum "$daemon_path" | awk '{print $1}')"
apk_sha="$(sha256sum "$apk_path" | awk '{print $1}')"
metadata_sha="$(sha256sum "$metadata_path" | awk '{print $1}')"
cat >"$session_file" <<SESSION
format=freezeit-build-session-v1
sessionId=$session_id
gitCommit=$end_commit
releaseKind=$RELEASE_KIND
daemonPath=$daemon_path
daemonSha256=$daemon_sha
apkPath=$apk_path
apkSha256=$apk_sha
apkMetadataPath=$metadata_path
apkMetadataSha256=$metadata_sha
SESSION
chmod 0600 "$session_file"
session_sha="$(sha256sum "$session_file" | awk '{print $1}')"

DAEMON="$daemon_path" \
APK="$apk_path" \
APK_METADATA="$metadata_path" \
EXPECTED_VERSION="$EXPECTED_VERSION" \
EXPECTED_VERSION_CODE="$EXPECTED_VERSION_CODE" \
RELEASE_KIND="$RELEASE_KIND" \
ALLOW_DIRTY="$ALLOW_DIRTY" \
BUILD_SESSION_ROOT="$BUILD_SESSION_ROOT" \
FREEZEIT_BUILD_SESSION_FILE="$session_file" \
FREEZEIT_BUILD_SESSION_ID="$session_id" \
FREEZEIT_BUILD_SESSION_SHA256="$session_sha" \
  "$ROOT/scripts/package-release.sh"
