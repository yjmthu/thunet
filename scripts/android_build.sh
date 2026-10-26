#!/usr/bin/env bash
# Build the Rust core into android/app/src/main/jniLibs.
#
#   scripts/android_build.sh [debug|release] [abi...]
#
# Defaults: profile `debug`, ABIs `arm64-v8a x86_64`; the release profile
# defaults to `arm64-v8a` only. Called by the Gradle `cargoNdk*` tasks and by
# `pixi run -e android apk-debug`, so both entry points use one SDK lookup.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

profile="${1:-debug}"
shift || true
abis=("$@")
if [ "${#abis[@]}" -eq 0 ]; then
  if [ "$profile" = release ]; then
    abis=(arm64-v8a)
  else
    abis=(arm64-v8a x86_64)
  fi
fi

# cargo lives in rustup's home even when the caller (Gradle, an IDE) did not
# inherit a login shell's PATH.
case ":$PATH:" in
  *":$HOME/.cargo/bin:"*) ;;
  *) PATH="$HOME/.cargo/bin:$PATH" ;;
esac
export PATH

# cargo-ndk resolves the NDK through ANDROID_NDK_HOME/ANDROID_NDK_ROOT or the
# newest NDK under ANDROID_HOME. `sdk.dir` in local.properties is the same
# answer Gradle and Android Studio already use.
if [ -z "${ANDROID_HOME:-}" ] && [ -z "${ANDROID_NDK_HOME:-}" ]; then
  local_props="$root/android/local.properties"
  if [ -f "$local_props" ]; then
    ANDROID_HOME="$(sed -n 's/^sdk\.dir=//p' "$local_props")"
    export ANDROID_HOME
  fi
fi
if [ -z "${ANDROID_HOME:-}" ] && [ -z "${ANDROID_NDK_HOME:-}" ]; then
  ANDROID_HOME="$HOME/Android/Sdk"
  export ANDROID_HOME
fi

targets=()
for abi in "${abis[@]}"; do
  targets+=(-t "$abi")
done

cargo_args=(build)
if [ "$profile" = release ]; then
  cargo_args+=(--release)
fi

echo "cargo ndk ${targets[*]} -P 33 -o android/app/src/main/jniLibs ${cargo_args[*]}"
exec cargo ndk "${targets[@]}" -P 33 -o android/app/src/main/jniLibs "${cargo_args[@]}"
