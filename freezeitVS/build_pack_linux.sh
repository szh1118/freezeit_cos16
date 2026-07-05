#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${ROOT_DIR}/.." && pwd)"
MAGISK_DIR="${ROOT_DIR}/magisk"
RELEASE_DIR="${RELEASE_DIR:-${REPO_ROOT}/freezeitRelease}"
APK_DIR="${REPO_ROOT}/freezeitApp/app/build/outputs/apk/release"
APK_PATH="${APK_PATH:-}"

require_file() {
  local path="$1"
  if [[ ! -f "${path}" ]]; then
    echo "Missing required file: ${path}" >&2
    exit 1
  fi
}

require_dir() {
  local path="$1"
  if [[ ! -d "${path}" ]]; then
    echo "Missing required directory: ${path}" >&2
    exit 1
  fi
}

require_dir "${MAGISK_DIR}"
require_dir "${RELEASE_DIR}"
require_file "${MAGISK_DIR}/module.prop"
require_file "${MAGISK_DIR}/freezeitARM64"

if [[ -z "${APK_PATH}" ]]; then
  mapfile -t apk_candidates < <(find "${APK_DIR}" -maxdepth 1 -type f -name '*.apk' | sort)
  if [[ "${#apk_candidates[@]}" -ne 1 ]]; then
    echo "Expected exactly one release APK in ${APK_DIR}; found ${#apk_candidates[@]}" >&2
    exit 1
  fi
  APK_PATH="${apk_candidates[0]}"
fi

require_file "${APK_PATH}"

module_id="$(awk -F= '$1 == "id" {print $2}' "${MAGISK_DIR}/module.prop")"
version="$(awk -F= '$1 == "version" {print $2}' "${MAGISK_DIR}/module.prop")"
version_code="$(awk -F= '$1 == "versionCode" {print $2}' "${MAGISK_DIR}/module.prop")"

if [[ -z "${module_id}" || -z "${version}" || -z "${version_code}" ]]; then
  echo "module.prop must define id, version, and versionCode" >&2
  exit 1
fi

cp "${ROOT_DIR}/changelog.txt" "${MAGISK_DIR}/changelog.txt"
cp "${APK_PATH}" "${MAGISK_DIR}/freezeit.apk"

zip_name="${module_id}_oneplus13_android16_selfuse_v${version}_${version_code}.zip"
zip_path="${RELEASE_DIR}/${zip_name}"
rm -f "${zip_path}"

(
  cd "${MAGISK_DIR}"
  shopt -s dotglob nullglob
  module_entries=(*)
  if [[ "${#module_entries[@]}" -eq 0 ]]; then
    echo "No files to package in ${MAGISK_DIR}" >&2
    exit 1
  fi
  if command -v zip >/dev/null 2>&1; then
    zip -qr "${zip_path}" "${module_entries[@]}"
  else
    bsdtar --format zip -cf "${zip_path}" "${module_entries[@]}"
  fi
)

echo "Packaged ${zip_path}"
