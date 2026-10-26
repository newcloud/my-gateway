#!/usr/bin/env bash
# 编译 my_gateway 动态模块，并拷贝到 nginx/modules/。
# 依赖：scripts/build-nginx.sh 已经跑过（需要 objs/ 里的 ngx_auto_config.h）。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NGINX_SRC="${NGINX_SOURCE_DIR:-$ROOT/.build/nginx-1.28.0}"

if [ ! -f "$NGINX_SRC/objs/Makefile" ]; then
  echo "找不到 $NGINX_SRC/objs/Makefile，请先运行 scripts/build-nginx.sh" >&2
  exit 1
fi

export NGINX_SOURCE_DIR="$NGINX_SRC"

# bindgen 需要 libclang；本机没有系统 clang，用 .build/llvm 里的本地副本
LOCAL_LLVM="$ROOT/.build/llvm/usr/lib64"
if [ -d "$LOCAL_LLVM" ]; then
  export LIBCLANG_PATH="$LOCAL_LLVM"
  [ -e "$LOCAL_LLVM/libclang.so" ] || ln -sf "$LOCAL_LLVM/libclang.so.21.1" "$LOCAL_LLVM/libclang.so"
  # 本地解包的 clang 找不到内置头文件，显式补上 resource dir
  CLANG_INC="$ROOT/.build/llvm/usr/lib/clang/21/include"
  if [ -d "$CLANG_INC" ]; then
    export BINDGEN_EXTRA_CLANG_ARGS="${BINDGEN_EXTRA_CLANG_ARGS:-} -I$CLANG_INC"
  fi
fi

echo "==> cargo build (release, NGINX_SOURCE_DIR=$NGINX_SOURCE_DIR)"
cargo build --release -p my-gateway --manifest-path "$ROOT/Cargo.toml"

mkdir -p "$ROOT/nginx/modules"
cp -f "$ROOT/target/release/libmy_gateway.so" "$ROOT/nginx/modules/libmy_gateway.so"
cp -f "$ROOT/deploy/nginx.conf" "$ROOT/nginx/conf/nginx.conf"

ls -l "$ROOT/nginx/modules/libmy_gateway.so"
echo "==> 模块已就绪：nginx/modules/libmy_gateway.so"
