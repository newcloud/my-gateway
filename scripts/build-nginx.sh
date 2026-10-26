#!/usr/bin/env bash
# 在项目本地编译 nginx（无需 root），供 my_gateway 动态模块加载。
# 版本必须与 Cargo.toml 里 nginx-sys 的 vendored 版本一致（1.28.x）。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${NGINX_VERSION:-1.28.0}"
SRC="$ROOT/.build/nginx-$VERSION"
PREFIX="$ROOT/nginx"

mkdir -p "$ROOT/.build" "$PREFIX/logs/tmp" "$PREFIX/modules"

if [ ! -d "$SRC" ]; then
  echo "==> 下载 nginx $VERSION"
  curl -fSL "https://nginx.org/download/nginx-$VERSION.tar.gz" -o "$ROOT/.build/nginx-$VERSION.tar.gz"
  tar -xzf "$ROOT/.build/nginx-$VERSION.tar.gz" -C "$ROOT/.build"
fi

if [ ! -f "$SRC/objs/Makefile" ]; then
  echo "==> configure（--with-compat 是动态模块 ABI 兼容的前提）"
  ( cd "$SRC" && ./configure \
      --prefix="$PREFIX" \
      --with-compat \
      --with-http_ssl_module \
      --with-http_v2_module )
fi

echo "==> make"
make -C "$SRC" -j"$(nproc)"

echo "==> make install -> $PREFIX"
make -C "$SRC" install

mkdir -p "$PREFIX/logs/tmp" "$PREFIX/modules"
cp -f "$ROOT/deploy/nginx.conf" "$PREFIX/conf/nginx.conf"

"$PREFIX/sbin/nginx" -v
echo "==> 完成。源码目录（模块编译需要）：$SRC"
