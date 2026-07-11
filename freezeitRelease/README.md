# Freezeit 自用版发布目录

本目录保存已验证的 Magisk 发布包与更新元数据，目标环境为 OnePlus 13（CPH2653）的 ColorOS/OxygenOS Android 16。

## Rust-only 发布规则

从 `3.3.0SelfUse`（`303000`）开始，正式版本仅支持 ARM64，并且只打包一个名为 `freezeit` 的 Rust 守护进程。守护进程源码位于 `freezeitDaemon/`，Manager 源码位于 `freezeitApp/`。旧 C++、x64 载荷和其他守护进程名称均不能作为发布输入，顶层 `magisk/` 模板也不保存二进制文件或 APK。

`3.3.1SelfUse`（`303001`）是前台恢复修复版本。

使用 `scripts/build-release.sh` 完成构建，或通过 `scripts/package-release.sh` 打包已核验的预构建文件。所有候选包都必须通过 `scripts/validate-release-zip.sh`，包括版本一致性、唯一 daemon、AArch64 ELF、ZIP 路径安全、完整载荷 SHA-256 和 provenance 检查。

## 发布门禁

`freezeitRelease/update.json` 描述已验证的 `freezeit_oneplus13_android16_selfuse_v3.3.1SelfUse_303001.zip`。

正式更新元数据必须满足：

- 对应的本地 ZIP 存在且通过完整校验。
- `zipSha256` 与最终发布文件完全一致。
- Git 工作树干净，provenance 指向准确提交。
- APK、模块和更新元数据版本一致。

脏工作树只能生成带源码快照、补丁和状态摘要的测试候选包，不能标记为 `released`。

## GPL-3.0 源码说明

Rust crate 声明 `GPL-3.0-or-later`。每个新发布包都会在 `provenance.txt` 中记录源码提交、Rust 源码目录、Manager 源码目录、目标三元组和构建产物 SHA-256，并包含 `LICENSE` 与指向准确源码提交的 `SOURCE_OFFER`。重新分发时必须保留对应源码可用性和许可证声明。
