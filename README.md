# freezeit_cos16

Freezeit / 冻它 的 COS16 自用维护版。

This workspace keeps the original Android manager, LSPosed module surface, and
Magisk packaging layout while moving new daemon control logic into
`freezeitDaemon/`. The verified target is OnePlus CPH2653 / Android 16 /
ColorOS 16.

Remote: https://github.com/szh1118/freezeit_cos16

## Current Self-Use Release

- Module version: `3.2.4SelfUse` / versionCode `302004`
- Release zip:
  `freezeitRelease/freezeit_oneplus13_android16_selfuse_v3.2.4SelfUse_302004.zip`
- Target device: OnePlus 13 / CPH2653 / CPH2653EEA
- Target system: ColorOS 16 / Android 16
- Root/Xposed: Magisk or KernelSU with LSPosed IT v2.1.0-it / Modern Xposed API 102

## 3.2.4SelfUse Changes

- Abnormal-thaw audits now directly requeue runnable background apps for freezing instead of marking them as temporary foreground apps.
- This covers daemon startup and screen-off standby cases where no foreground-app refresh happens after the audit.

## 3.2.3SelfUse Changes

- Abnormal-thaw audits now continue while the device is in screen-off standby/doze, so boot-started background apps are requeued without opening the log page.
- Keeps the 3.2.2 startup audit schedule: every 60 seconds during the first 15 minutes, then the configured Regular Refreeze interval.

## 3.2.2SelfUse Changes

- Legacy freezer now rechecks abnormal thawed background processes every 60 seconds during the first 15 minutes after daemon startup.
- After the startup window, the same audit follows the configured Regular Refreeze interval instead of a hard-coded 1 hour.
- Manual log-page freeze-status checks still force the next audit immediately.

## 3.2.1SelfUse Changes

- Manager log view follows the newest log output at the bottom by default.
- Manager app-state page now shows foreground/background, frozen state, process count, and freeze time instead of CPU time.
- Legacy native freeze-status aggregation keeps frozen state when an app has multiple matching processes.
- Release metadata and packaged Magisk module are bumped to `3.2.1SelfUse`.

## Layout

- `freezeitApp/`: Android manager APK and LSPosed/Xposed hook code.
- `freezeitDaemon/`: Rust daemon rewrite and host/device tests.
- `freezeitVS/`: legacy native service reference and Magisk module source.
- `freezeitRelease/`: release metadata and selected packaged artifacts.
- `specs/`: Spec Kit feature specs, tasks, and validation evidence.
- `scripts/`: repo-level build, packaging, and validation helpers.

## Install

1. Flash `freezeitRelease/freezeit_oneplus13_android16_selfuse_v3.2.4SelfUse_302004.zip` in Magisk.
2. Enable the Freezeit module in LSPosed.
3. Select at least these LSPosed scopes:
   - System framework / `system`
   - Freezeit manager / `io.github.jark006.freezeit`
4. Reboot.

## Build And Validate

- Target-device claims are limited to the recorded CPH2653 Android 16 baseline.
- Rust host checks run with `freezeitDaemon/scripts/test-host.sh`.
- Android manager compile checks run from `freezeitApp/` with
  `./gradlew :app:compileDebugJavaWithJavac`.
- Legacy package helper path is `freezeitVS/build_pack_linux.sh`.
- Release zips must pass `scripts/validate-release-zip.sh`.
- Device validation evidence lives under
  `specs/002-modern-freezer-rewrite/evidence/`.

Do not treat unchecked tasks in `specs/002-modern-freezer-rewrite/tasks.md` as
complete release behavior.
