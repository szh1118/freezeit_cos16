#!/usr/bin/env sh
set -eu

cd "$(dirname "$0")/.."

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
explicit_ndk="$tmp/explicit-sdk/ndk/26.3.11579264"
home_ndk="$tmp/home/Android/Sdk/ndk/99.0.0"
mkdir -p "$tmp/bin" \
  "$explicit_ndk/toolchains/llvm/prebuilt/linux-x86_64/bin" \
  "$home_ndk/toolchains/llvm/prebuilt/linux-x86_64/bin"
printf '%s\n' '#!/usr/bin/env sh' 'printf "%s\\n" aarch64-linux-android' >"$tmp/bin/rustup"
printf '%s\n' \
  '#!/usr/bin/env sh' \
  '[ "$ANDROID_NDK_HOME" = "$EXPECTED_NDK" ] || exit 91' \
  '[ "$CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER" = "$EXPECTED_NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android31-clang" ] || exit 92' \
  '[ "$1" = build ] && [ "$2" = --release ] && [ "$3" = --target ] && [ "$4" = aarch64-linux-android ] || exit 93' >"$tmp/bin/cargo"
printf '%s\n' '#!/usr/bin/env sh' 'exit 0' >"$explicit_ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android31-clang"
printf '%s\n' '#!/usr/bin/env sh' 'exit 0' >"$home_ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android31-clang"
chmod 0755 "$tmp/bin/cargo" "$tmp/bin/rustup" \
  "$explicit_ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android31-clang" \
  "$home_ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android31-clang"
HOME="$tmp/home" \
PATH="$tmp/bin:/usr/bin:/bin" \
ANDROID_SDK_ROOT="$tmp/explicit-sdk" \
ANDROID_HOME='' \
ANDROID_NDK_HOME='' \
EXPECTED_NDK="$explicit_ndk" \
./scripts/build-android.sh

cargo fmt --check
cargo test --target x86_64-unknown-linux-gnu
