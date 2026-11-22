#!/usr/bin/env bash
#
# Builds the debug APK of PakePlus for Android.
#
# The script performs the same steps as the continuous integration workflow
# (.github/workflows/android.yml), it needs the same tools:
#
#   * a JDK 17, JAVA_HOME has to point to it,
#   * the Android SDK with the platform and the NDK of the Tauri CLI,
#     ANDROID_SDK_ROOT and NDK_HOME have to point to them,
#   * the Rust toolchain with the targets for Android:
#       rustup target add aarch64-linux-android armv7-linux-androideabi \
#           i686-linux-android x86_64-linux-android
#   * pnpm, the frontend dependencies are installed with the lock file.
#
# The resulting APK is written to
# src-tauri/gen/android/app/build/outputs/apk/debug.

set -euo pipefail

cd "$(dirname "$0")/.."

export ANDROID_API_LEVEL="${ANDROID_API_LEVEL:-36}"
export ANDROID_NDK_VERSION="${ANDROID_NDK_VERSION:-29.0.13846066}"

if [ -z "${NDK_HOME:-}" ] && [ -n "${ANDROID_SDK_ROOT:-}" ]; then
  export NDK_HOME="${ANDROID_SDK_ROOT}/ndk/${ANDROID_NDK_VERSION}"
fi

echo "==> installing the frontend dependencies"
pnpm install --frozen-lockfile

echo "==> preparing the Android project"
pnpm tauri android init --ci --skip-targets-install

echo "==> building the debug APK"
pnpm tauri android build --ci --debug --apk

echo "==> the APK is in src-tauri/gen/android/app/build/outputs/apk/debug"
