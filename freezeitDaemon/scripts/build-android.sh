#!/usr/bin/env sh
set -eu

cd "$(dirname "$0")/.."

if [ -d "$HOME/.cargo/bin" ]; then
    PATH="$HOME/.cargo/bin:$PATH"
    export PATH
fi

if [ -z "${ANDROID_HOME:-}" ] && [ -z "${ANDROID_SDK_ROOT:-}" ] && [ -d "$HOME/Android/Sdk" ]; then
    ANDROID_HOME="$HOME/Android/Sdk"
    export ANDROID_HOME
fi

if [ -z "${ANDROID_SDK_ROOT:-}" ] && [ -n "${ANDROID_HOME:-}" ]; then
    ANDROID_SDK_ROOT="$ANDROID_HOME"
    export ANDROID_SDK_ROOT
fi

sdk_root="${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}"
if [ -z "${ANDROID_NDK_HOME:-}" ] && [ -n "$sdk_root" ] && [ -d "$sdk_root/ndk" ]; then
    ANDROID_NDK_HOME="$(find "$sdk_root/ndk" -mindepth 1 -maxdepth 1 -type d | sort -V | tail -n 1)"
    export ANDROID_NDK_HOME
fi

if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo is required" >&2
    exit 1
fi

if command -v rustup >/dev/null 2>&1; then
    if ! rustup target list --installed | grep -qx 'aarch64-linux-android'; then
        rustup target add aarch64-linux-android
    fi
fi

if command -v cargo-ndk >/dev/null 2>&1; then
    cargo ndk --target arm64-v8a --platform 31 build --release
else
    if [ -z "${ANDROID_NDK_HOME:-}" ]; then
        echo "ANDROID_NDK_HOME is required when cargo-ndk is unavailable" >&2
        exit 1
    fi
    NDK_BIN="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"
    ANDROID_LINKER="$NDK_BIN/aarch64-linux-android31-clang"
    if [ ! -x "$ANDROID_LINKER" ]; then
        echo "Android linker not found: $ANDROID_LINKER" >&2
        exit 1
    fi
    CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$ANDROID_LINKER"
    CC_aarch64_linux_android="$ANDROID_LINKER"
    AR_aarch64_linux_android="$NDK_BIN/llvm-ar"
    export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER
    export CC_aarch64_linux_android
    export AR_aarch64_linux_android
    cargo build --release --target aarch64-linux-android
fi
