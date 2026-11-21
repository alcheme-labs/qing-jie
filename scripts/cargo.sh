#!/bin/sh
set -eu
PROJECT_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$PROJECT_ROOT"
LOCAL_TOOLCHAIN="$PROJECT_ROOT/.build-tools/rustup/toolchains/stable-aarch64-apple-darwin/bin"
if [ -d "$PROJECT_ROOT/.build-tools/cargo" ]; then
    export CARGO_HOME="$PROJECT_ROOT/.build-tools/cargo"
fi
if [ -x "$LOCAL_TOOLCHAIN/cargo" ]; then
    export RUSTC="$LOCAL_TOOLCHAIN/rustc"
    export RUSTDOC="$LOCAL_TOOLCHAIN/rustdoc"
    CARGO_BIN="$LOCAL_TOOLCHAIN/cargo"
else
    CARGO_BIN=cargo
fi
export MACOSX_DEPLOYMENT_TARGET=13.0
exec "$CARGO_BIN" "$@"
