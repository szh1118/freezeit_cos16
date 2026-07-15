# Freezeit 自用版发布目录

本目录保存已验证的 Magisk 发布包与更新元数据，目标环境为 OnePlus 13（CPH2653）的 ColorOS/OxygenOS Android 16。

## 仅 Rust 发布规则

从 `3.3.0SelfUse`（`303000`）开始，正式版本仅支持 ARM64，并且只打包一个名为 `freezeit` 的 Rust 守护进程。守护进程源码位于 `freezeitDaemon/`，管理器源码位于 `freezeitApp/`。旧 C++、x64 载荷和其他守护进程名称均不能作为发布输入，顶层 `magisk/` 模板也不保存二进制文件或 APK。

`3.3.4SelfUse`（`303004`）修复 Binder 调试节点不可读时把所有候选应用永久卡在等待冻结状态的问题，保留明确活跃事务的延迟策略，并让 Binder freezer 降级后的 SIGSTOP 回退准确记录实际后端。Android 16 上尚未实现的 `BroadcastQueueModernImpl` 广播抑制被标记为可选降级，核心冻结控制继续运行，但该广播拦截优化暂不可用。该版本同时收敛控制循环、Xposed bridge、配置协议、管理器并发和发布验证链的稳定性修复。

正式包只能由 `scripts/build-release.sh` 的同次构建会话生成。`scripts/package-release.sh` 可用于候选包，但不能把外部预构建文件声明为正式产物。所有包都必须通过 `scripts/validate-release-zip.sh`，包括版本一致性、唯一守护进程、AArch64 ELF、ZIP 路径安全、完整载荷 SHA-256 和来源信息检查。

## 发布门禁

`3.3.4SelfUse`（`303004`）正在按同次构建会话、证书和真机验证门禁生成正式包；在 ZIP 上传并完成 SHA-256 校验前，`freezeitRelease/update.json` 继续指向上一版。

正式更新元数据必须满足：

- 对应的本地 ZIP 存在且通过完整校验。
- `zipSha256` 与最终发布文件完全一致。
- Git 工作树干净，来源信息指向准确提交。
- APK、模块和更新元数据版本一致。
- 正式签名密钥存在，且 APK 签名证书 SHA-256 与发布配置一致。
- 守护进程、APK 和元数据来自同一次受控构建会话。

脏工作树只能生成带源码快照、补丁和状态摘要的测试候选包，不能标记为 `released`。

## GPL-3.0 源码说明

Rust crate 声明 `GPL-3.0-or-later`。每个新发布包都会在 `provenance.txt` 中记录源码提交、Rust 源码目录、管理器源码目录、目标三元组、构建会话、APK 签名证书和构建产物 SHA-256，并包含 `LICENSE` 与指向准确源码提交的 `SOURCE_OFFER`。重新分发时必须保留对应源码可用性和许可证声明。
