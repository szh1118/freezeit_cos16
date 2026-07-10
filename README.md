# Freezeit CPH2653 Android 16 Self-Use

This workspace maintains a device-scoped Freezeit build for the recorded
OnePlus/ColorOS Android 16 baseline. It is not a generic Android release.

## Release Model

- Planned module version: `3.3.0SelfUse` / versionCode `303000`.
- Published metadata remains on the last validated release in
  `freezeitRelease/update.json` until a complete `3.3.0SelfUse` archive passes
  the release validator.
- New releases are Rust-only and ARM64-only. The package contains exactly one
  daemon named `freezeit`, built from `freezeitDaemon/` for
  `aarch64-linux-android`.
- `freezeitARM64`, `freezeitX64`, `freezeitRustARM64`, and `freezeitRustX64`
  payloads are rejected. There is no C++ or x64 fallback.
- The source template is `magisk/`; it must not contain binaries or APKs.
- The active tree contains no legacy C++ daemon or legacy native build chain.

## Build And Package

Run the unified release entry point:

```sh
scripts/build-release.sh
```

It builds the Rust ARM64 daemon and release APK, verifies both use
`3.3.0SelfUse` / `303000`, and delegates to `scripts/package-release.sh`.
Packaging happens in disposable `.release-staging/` storage and emits
`freezeitRelease/freezeit_oneplus13_android16_selfuse_v3.3.0SelfUse_303000.zip`.

For prebuilt artifacts, set `DAEMON`, `APK`, and the matching Gradle
`output-metadata.json` explicitly:

```sh
DAEMON=/path/to/aarch64/freezeit \
APK=/path/to/freezeit.apk \
APK_METADATA=/path/to/output-metadata.json \
scripts/package-release.sh
```

The packager and validator enforce version consistency, one daemon, one APK,
AArch64 ELF identity, complete payload SHA256 checks, safe ZIP paths, and
`provenance.txt` source records. Every archive includes `LICENSE` and an exact
commit URL in `SOURCE_OFFER`.

Packaging rejects a dirty Git tree by default. Dirty builds are allowed only
as explicit test candidates with `RELEASE_KIND=candidate ALLOW_DIRTY=1`; those
ZIPs embed `source.patch`, `source-state.txt`, and `source-snapshot.tar.gz`, all
bound by SHA-256 values in `provenance.txt`. Final `released` packages must be
clean. Released update metadata must name a local ZIP that passes the validator
and must bind its exact digest through `zipSha256`.

## Validation

```sh
scripts/test-release-pipeline.sh
scripts/test-release-metadata.sh planned
scripts/validate-release-zip.sh /path/to/release.zip 3.3.0SelfUse 303000
```

`freezeitRelease/update.json` must be changed only after the final command
passes for the exact archive that will be published. Then validate published
metadata with `scripts/test-release-metadata.sh released 3.3.0SelfUse 303000`.

## Source And License

The Rust daemon source is in `freezeitDaemon/`, and the Android manager source
is in `freezeitApp/`. The Rust crate declares `GPL-3.0-or-later`; release
provenance identifies those source directories and the Git commit used for the
package. Preserve the corresponding GPL-3.0 source offer and license notices
when redistributing binaries.

## 来源

基于 [jark006/freezeitVS](https://github.com/jark006/freezeitVS)（已停更，仓库实际采用 GPL-3.0）。

原作者公开鼓励继续维护、修改与再分发；相关公开回复作为项目历史背景保留。本仓库继续遵守现有 GPL-3.0 许可，不将该回复解释为对既有代码的 MIT 重新许可。

## 本仓库做了什么

- 适配 ColorOS 16 / Android 16 / Xposed API 102
- 核心从 C++ 重构为 Rust
- 面向自用的 hotfix 版本线（SelfUse）

## Safety Boundary

This self-use module controls selected background application runtime after
root, hook, freezer, foreground, delay, and idle checks pass. It is not a
malware scanner, sandbox, exploit mitigator, or system trust boundary.
