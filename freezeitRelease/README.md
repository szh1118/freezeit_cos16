# Freezeit self-use release workspace

This directory stores validated self-use Magisk archives and update metadata
for the recorded OnePlus/ColorOS Android 16 baseline.

## Rust-Only Release Policy

Starting with version `3.3.0SelfUse` / `303000`, releases are ARM64-only
and package exactly one Rust daemon as `freezeit`. The daemon source is
`freezeitDaemon/`; the manager source is `freezeitApp/`. Legacy C++ and x64
payloads are not release inputs, and the top-level `magisk/` template contains
no binaries or APKs.

Version `3.3.1SelfUse` / `303001` is the foreground-resume hotfix release.

Build with `scripts/build-release.sh`, or package verified prebuilt artifacts
with `scripts/package-release.sh`. Every candidate must pass
`scripts/validate-release-zip.sh`, including version consistency, unique daemon,
AArch64 ELF, safe ZIP paths, complete payload SHA256, and provenance checks.

## Publication Gate

`freezeitRelease/update.json` describes the validated
`freezeit_oneplus13_android16_selfuse_v3.3.1SelfUse_303001.zip` hotfix release.
Existing release ZIPs are retained.
Released metadata also requires a `zipSha256` equal to that local ZIP after it
passes `scripts/validate-release-zip.sh`; metadata cannot advertise a missing or
unvalidated artifact. Dirty trees may produce test candidates only, with an
embedded source snapshot and patch/state digests, and can never be published as
`released`.

## GPL-3.0 Source

The Rust crate declares `GPL-3.0-or-later`. Each new archive records the Git
commit, Rust source directory, manager source directory, target triple, and
artifact SHA256 values in `provenance.txt`. Archives also include `LICENSE` and
`SOURCE_OFFER`, whose URL names the exact source commit. Redistributors must
preserve the corresponding GPL-3.0 source availability and notices.
