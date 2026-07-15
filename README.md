# Freezeit CPH2653 Android 16 自用版

本仓库维护面向 OnePlus 13（CPH2653）与 ColorOS/OxygenOS Android 16 的 Freezeit 自用版本，不是通用 Android 发行版。运行时会检查实际系统能力；未经验证的环境默认谨慎降级，而不是仅凭机型字符串宣称兼容。

## 当前版本

- 模块版本：`3.3.4SelfUse`，版本号：`303004`。
- `freezeitRelease/update.json` 在 ZIP 完成同次构建、完整校验并上传前继续指向已验证的上一版。
- 新版本仅支持 ARM64，且只包含一个由 `freezeitDaemon/` 为 `aarch64-linux-android` 构建的 Rust 守护进程 `freezeit`。
- 不提供 C++、x64 或旧守护进程回退。
- Magisk 源模板位于 `magisk/`，仓库中的模板不保存守护进程、APK 等构建产物。

`3.3.4SelfUse` 修复 Binder 调试信息不可读时把所有候选应用永久卡在等待冻结状态的问题，并保留明确的活跃 Binder 事务阻断；在 Binder freezer 降级时，实际走 SIGSTOP 回退的操作会准确记录为 `signal.stop`。Android 16 上暂未适配的 `BroadcastQueueModernImpl` 广播抑制按可选能力降级，核心冻结控制不会因此关闭。该版本同时收敛守护进程、Xposed bridge、控制状态恢复、配置协议、管理器并发与发布验证链的稳定性修复。

## 构建与打包

统一发布入口：

```sh
scripts/build-release.sh
```

脚本会构建 ARM64 Rust 守护进程与正式版管理器，核对二者版本，随后调用 `scripts/package-release.sh`。打包在临时的 `.release-staging/` 中进行，输出：

```text
freezeitRelease/freezeit_oneplus13_android16_selfuse_v3.3.4SelfUse_303004.zip
```

脏工作树测试包可显式提供已构建并核验的文件：

```sh
RELEASE_KIND=candidate ALLOW_DIRTY=1 \
DAEMON=/path/to/aarch64/freezeit \
APK=/path/to/freezeit.apk \
APK_METADATA=/path/to/output-metadata.json \
scripts/package-release.sh
```

发布脚本会检查：

- APK、模块与更新元数据版本一致。
- ZIP 中恰好包含一个 ARM64 ELF 守护进程和一个 APK。
- ZIP 路径安全，载荷 SHA-256 完整。
- 正式包必须由 `scripts/build-release.sh` 的同次构建会话生成，不能把任意预构建文件声明为当前提交产物。
- 正式包必须使用指定签名证书；APK 签名证书 SHA-256 会写入并校验于 `provenance.txt`。
- `provenance.txt` 记录源码提交、目标架构、构建会话和构建产物来源摘要。
- `SOURCE_OFFER` 指向对应提交的完整源码。

正式发布包要求 Git 工作树干净、配置正式签名密钥，并设置 `FREEZEIT_EXPECTED_APK_SIGNER_SHA256`。仅测试候选包可以设置 `RELEASE_KIND=candidate ALLOW_DIRTY=1`；候选包会额外保存源码补丁、状态和快照，不能作为正式更新发布。

## 验证

```sh
scripts/test-release-pipeline.sh
scripts/test-release-metadata.sh planned
scripts/validate-release-zip.sh /path/to/release.zip 3.3.4SelfUse 303004
```

只有待发布 ZIP 通过完整校验后，才能修改 `freezeitRelease/update.json`。发布后再执行：

```sh
scripts/test-release-metadata.sh released 3.3.4SelfUse 303004
```

## 项目来源

基于 [jark006/freezeitVS](https://github.com/jark006/freezeitVS)（已停止维护，原仓库采用 GPL-3.0）。

原作者曾公开鼓励继续维护、修改与再分发，该回复作为项目历史背景保留。本仓库继续遵守 GPL-3.0，不将该回复解释为对既有代码的 MIT 重新许可。

## 本仓库的改动

- 适配 ColorOS 16 / OxygenOS 16、Android 16 与 Xposed API 102。
- 核心守护进程由 C++ 重构为 Rust。
- 使用运行时能力探测替代仅依赖机型、指纹和版本号的硬编码判断。
- 建立仅 Rust 的 ARM64 构建、验证和发布链。
- 维护面向自用场景的 SelfUse 修复版本线。

## 源码与许可证

Rust 守护进程位于 `freezeitDaemon/`，Android 管理器位于 `freezeitApp/`。Rust crate 声明 `GPL-3.0-or-later`；仓库根目录和发布包均保留许可证及对应源码说明。重新分发二进制文件时，必须继续提供相应源码并保留许可证声明。

## 安全边界

本模块只有在 root、hook、freezer、前台状态、延迟与 idle 等检查通过后，才控制选定后台应用。它不是恶意软件扫描器、应用沙箱、漏洞缓解工具或系统信任边界。启用前请确保具备 Magisk 模块禁用、卸载与救援能力。
