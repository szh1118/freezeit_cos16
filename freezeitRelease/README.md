# Freezeit self-use release workspace

This directory stores legacy release artifacts plus modern rewrite test zips for
the verified CPH2653 Android 16 self-use target. Public compatibility is not
claimed by the modern rewrite until the release validation and 24-hour soak
tasks are checked in `specs/002-modern-freezer-rewrite/tasks.md`.

Use `scripts/package-release.sh` from the repository root after producing the
Rust daemon and manager APK artifacts. Validate any candidate zip with
`scripts/validate-release-zip.sh`.

## Current Modern Rewrite Artifacts

- Release candidate zip:
  `freezeitRelease/freezeit_oneplus13_android16_selfuse_v3.2.1SelfUse_302001.zip`
- Source archive:
  `freezeitRelease/freezeit_3.2.0SelfUse_source.tar.gz`
- Build evidence:
  `specs/002-modern-freezer-rewrite/evidence/final-build.md`

These artifacts are not final release-complete until the unchecked target-device
validation, release validation, aggregate review/convergence, and 24-hour soak
tasks are complete.

## Self-Use Threat Boundary

`3.2.1SelfUse` is a background runtime-control build for the verified CPH2653
Android 16 baseline. It can reduce selected third-party app background execution
after hook/root/freezer readiness, foreground eligibility, configured delay, and
idle checks pass.

It is not a malware scanner, sandbox, exploit mitigator, or root/system trust
boundary. It does not prevent behavior before freeze, foreground behavior while
the app is being used, or activity from privileged ROM/root/system components.
Binder freezer support is reported as `untested` until a safe target ioctl probe
is implemented and validated.

# ❌ 本项目已停止维护 ❌

---

# FreezeitRelease 冻它模块发布页

**[面具模块]** 实现部分墓碑机制，自动暂停后台进程的运行。

**[MagiskModule]** Implement a partial tombstone mechanism to automatically suspend background processes.

### 相关链接

1. [项目开源地址](https://github.com/jark006/freezeitVS)

1. [管理器开源地址](https://github.com/jark006/freezeitapp)

1. [模块包发布地址](https://github.com/jark006/freezeitRelease)

### 其他链接

[教程 Tutorials](https://jark006.github.io/FreezeitIntroduction/) |
[酷安 @JARK006](https://www.coolapk.com/u/1212220) |
[QQ频道 冻它模块](https://qun.qq.com/qqweb/qunpro/share?_wv=3&_wwv=128&appChannel=share&inviteCode=1W6opB7&appChannel=share&businessType=9&from=246610&biz=ka) |
[Telegram Group](https://t.me/+sjDX1oTk31ZmYjY1) |
[蓝奏云 密码: dy6i](https://jark006.lanzout.com/b017oz9if) 
