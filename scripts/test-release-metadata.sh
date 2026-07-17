#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
update_json="${UPDATE_JSON:-$root/freezeitRelease/update.json}"
release_dir="${RELEASE_DIR:-$root/freezeitRelease}"
mode="${1:-planned}"
module_prop() {
  awk -F= -v key="$1" '$1 == key {print substr($0, index($0, "=") + 1); exit}' "$root/magisk/module.prop"
}
planned_version="${2:-$(module_prop version)}"
planned_code="${3:-$(module_prop versionCode)}"

fail() { echo "release metadata test failed: $*" >&2; exit 1; }
[[ "$mode" == planned || "$mode" == released ]] || fail "mode must be planned or released"
[[ "$planned_version" =~ ^[0-9A-Za-z][0-9A-Za-z._+-]{0,63}$ ]] || fail "invalid version"
[[ "$planned_code" =~ ^[0-9]{1,10}$ ]] || fail "invalid versionCode"

require_exact_line() {
  grep -Fqx "$2" "$root/$1" >/dev/null || fail "missing expected line in $1: $2"
}

require_text() {
  grep -F "$2" "$root/$1" >/dev/null || fail "missing expected text in $1: $2"
}

archive_prop() {
  local archive_path="$1"
  local key="$2"
  local values=()
  mapfile -t values < <(
    unzip -p "$archive_path" provenance.txt \
      | awk -F= -v key="$key" '$1 == key {print substr($0, index($0, "=") + 1)}'
  )
  [[ ${#values[@]} -eq 1 ]] || fail "released ZIP has invalid provenance field: $key"
  printf '%s\n' "${values[0]}"
}

require_exact_line magisk/module.prop "version=$planned_version"
require_exact_line magisk/module.prop "versionCode=$planned_code"

mapfile -t published < <(python3 - "$update_json" <<'PY'
import json
import sys
try:
    with open(sys.argv[1], encoding="utf-8") as stream:
        data = json.load(stream)
except (OSError, json.JSONDecodeError) as error:
    raise SystemExit(f"invalid update metadata JSON: {error}")
for key in ("version", "versionCode", "zipUrl", "zipSha256", "changelog"):
    if key not in data:
        raise SystemExit(f"missing update metadata key: {key}")
print(data["version"])
print(data["versionCode"])
print(data["zipUrl"])
print(data["zipSha256"])
PY
)
[[ ${#published[@]} -eq 4 ]] || fail "cannot parse update metadata"
[[ "${published[3]}" =~ ^[0-9A-Fa-f]{64}$ ]] || fail "invalid published zipSha256"

if [[ "$mode" == planned ]]; then
  [[ "${published[0]}" != "$planned_version" && "${published[1]}" != "$planned_code" ]] \
    || fail "planned version must not be advertised before artifact validation"
else
  [[ "${published[0]}" == "$planned_version" ]] || fail "released version mismatch"
  [[ "${published[1]}" == "$planned_code" ]] || fail "released versionCode mismatch"
  expected_zip="freezeit_oneplus13_android16_selfuse_v${planned_version}_${planned_code}.zip"
  [[ "${published[2]}" == *"/$expected_zip" ]] || fail "released zipUrl does not match version"
  local_zip="$release_dir/$expected_zip"
  [[ -f "$local_zip" ]] || fail "released metadata requires local ZIP: $local_zip"
  "$root/scripts/validate-release-zip.sh" "$local_zip" "$planned_version" "$planned_code" >/dev/null
  [[ "$(archive_prop "$local_zip" releaseKind)" == released ]] \
    || fail "released metadata requires provenance releaseKind=released"
  [[ "$(archive_prop "$local_zip" dirty)" == false ]] \
    || fail "released metadata requires clean provenance"
  actual_sha="$(sha256sum "$local_zip" | awk '{print $1}')"
  [[ "${published[3]}" == "$actual_sha" ]] || fail "released zipSha256 mismatch"
fi

require_text README.md "\`$planned_version\`"
require_text README.md "\`$planned_code\`"
require_text freezeitRelease/README.md "\`$planned_version\`"
require_text freezeitRelease/README.md "\`$planned_code\`"
require_text README.md 'GPL-3.0-or-later'
require_text freezeitRelease/README.md 'GPL-3.0-or-later'

echo "release metadata $mode: pass"
