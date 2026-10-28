#!/usr/bin/env bash
# 本地自签 HTTPS 契约源验收：gateway_info_url 的 `allow-localhost-ca` 选项。
#
# 覆盖：
#   - 只允许 https://localhost / 127.0.0.1 / [::1]；
#   - 带选项时关闭该网关的证书校验，能从本地自签 HTTPS 拉取契约；
#   - 不带选项时自签证书拉取失败，请求 503 fail-closed；
#   - 非本地地址、http://、未知选项在 nginx -t 阶段拒绝。
#
# 前置：./scripts/build-module.sh；需要 openssl 与 python3（离线可跑）。
set -uo pipefail

export NO_PROXY="127.0.0.1,localhost,::1"
export no_proxy="$NO_PROXY"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NGINX_BIN="$ROOT/nginx/sbin/nginx"
MODULE="$ROOT/nginx/modules/libmy_gateway.so"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/tls-local.XXXXXX")"
WITH="$TMP/with"
WITHOUT="$TMP/without"
PY_PORT=18443
WITH_PORT=18082
WITHOUT_PORT=18083
PY_PID=""

PASS=0
FAIL=0
CODE=""

ok() { printf '  \033[32mPASS\033[0m %s\n' "$1"; PASS=$((PASS + 1)); }
bad() { printf '  \033[31mFAIL\033[0m %s\n       %s\n' "$1" "$2"; FAIL=$((FAIL + 1)); }
section() { printf '\n\033[1m%s\033[0m\n' "$1"; }

cleanup() {
  for prefix in "$WITH" "$WITHOUT"; do
    if [ -f "$prefix/logs/nginx.pid" ]; then
      "$NGINX_BIN" -p "$prefix" -c conf/nginx.conf -s stop >/dev/null 2>&1 || true
    fi
  done
  [ -n "$PY_PID" ] && kill "$PY_PID" >/dev/null 2>&1
  rm -rf "$TMP"
}
trap cleanup EXIT

req() {
  local method="$1" url="$2"
  shift 2
  CODE="$(curl -sS -m 10 -X "$method" -o "$TMP/body" -D "$TMP/head" -w '%{http_code}' "$@" "$url" 2>/dev/null)" || CODE="000"
}

expect_code() {
  local name="$1" want="$2"
  if [ "$CODE" = "$want" ]; then
    ok "$name [$CODE]"
  else
    bad "$name" "期望 $want，实际 $CODE；body=$(head -c 200 "$TMP/body" 2>/dev/null)"
  fi
}

body_has() {
  local name="$1" needle="$2"
  if grep -q -- "$needle" "$TMP/body"; then ok "$name"; else bad "$name" "响应体缺少 '$needle'：$(head -c 200 "$TMP/body")"; fi
}

head_has() {
  local name="$1" needle="$2"
  if grep -qi -- "$needle" "$TMP/head"; then ok "$name"; else bad "$name" "响应头缺少 '$needle'：$(tr -d '\r' <"$TMP/head" | head -8)"; fi
}

log_has() {
  local name="$1" file="$2" needle="$3"
  if grep -qF -- "$needle" "$file" 2>/dev/null; then
    ok "$name"
  else
    bad "$name" "${file} 未找到 '$needle'"
  fi
}

section "0. 前置检查"
if [ ! -f "$MODULE" ]; then
  bad "模块存在" "缺少 $MODULE，请先运行 ./scripts/build-module.sh"
  printf '\n\033[1m结果：%d 通过，%d 失败\033[0m\n' "$PASS" "$FAIL"
  exit 1
fi
ok "模块存在"

for tool in openssl python3 curl; do
  if command -v "$tool" >/dev/null 2>&1; then ok "$tool 可用"; else bad "$tool 可用" "未安装"; fi
done

mkdir -p "$TMP/www" "$WITH/conf" "$WITH/logs" "$WITH/modules" "$WITHOUT/conf" "$WITHOUT/logs" "$WITHOUT/modules"
cp "$MODULE" "$WITH/modules/libmy_gateway.so"
cp "$MODULE" "$WITHOUT/modules/libmy_gateway.so"

# 自签证书（只给 127.0.0.1 用）
if openssl req -x509 -newkey rsa:2048 -nodes -keyout "$TMP/www/key.pem" -out "$TMP/www/cert.pem" \
  -days 1 -subj '/CN=127.0.0.1' -addext 'subjectAltName=IP:127.0.0.1' >"$TMP/openssl.log" 2>&1; then
  ok "生成自签证书"
else
  bad "生成自签证书" "$(head -3 "$TMP/openssl.log")"
fi

cat >"$TMP/www/contract.json" <<'JSON'
{"protocol":1,"version":"local-tls","routes":[{"path":"/contract.json","methods":["GET"]}]}
JSON

cat >"$TMP/www/https_server.py" <<'PY'
import http.server, ssl, sys, os
os.chdir(sys.argv[1])
httpd = http.server.HTTPServer(('127.0.0.1', int(sys.argv[2])), http.server.SimpleHTTPRequestHandler)
ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
ctx.load_cert_chain('cert.pem', 'key.pem')
httpd.socket = ctx.wrap_socket(httpd.socket, server_side=True)
httpd.serve_forever()
PY

python3 "$TMP/www/https_server.py" "$TMP/www" "$PY_PORT" >"$TMP/python.log" 2>&1 &
PY_PID=$!
for _ in $(seq 1 30); do
  curl -sk -o /dev/null "https://127.0.0.1:$PY_PORT/contract.json" && break
  sleep 0.1
done
if curl -sk -o /dev/null -w '%{http_code}' "https://127.0.0.1:$PY_PORT/contract.json" | grep -q '^200$'; then
  ok "本地自签 HTTPS 契约源就绪"
else
  bad "本地自签 HTTPS 契约源就绪" "$(head -3 "$TMP/python.log")"
  printf '\n\033[1m结果：%d 通过，%d 失败\033[0m\n' "$PASS" "$FAIL"
  exit 1
fi

write_conf() {
  local file="$1" port="$2" log="$3" flag="$4"
  cat >"$file" <<CONF
worker_processes 1;
error_log $log notice;
pid logs/nginx.pid;
load_module modules/libmy_gateway.so;
events { worker_connections 64; }
http {
    access_log off;
    gateway_diagnostics on;
    gateway_name local_gw;
    gateway_info_url local_gw https://127.0.0.1:$PY_PORT/contract.json $flag;
    server {
        listen 127.0.0.1:$port;
        location / {
            enable_gateway_by local_gw;
            proxy_pass https://127.0.0.1:$PY_PORT;
            proxy_ssl_verify off;
            proxy_set_header Host 127.0.0.1;
        }
    }
}
CONF
}

write_conf "$WITH/conf/nginx.conf" "$WITH_PORT" "logs/error.log" "allow-localhost-ca"
write_conf "$WITHOUT/conf/nginx.conf" "$WITHOUT_PORT" "logs/error.log" ""

section "1. allow-localhost-ca：从本地自签 HTTPS 拉取契约"
if "$NGINX_BIN" -p "$WITH" -c conf/nginx.conf -t >"$TMP/with-t.log" 2>&1; then
  ok "带 allow-localhost-ca 的配置可加载"
else
  bad "带 allow-localhost-ca 的配置可加载" "$(head -3 "$TMP/with-t.log")"
fi
log_has "  配置期输出安全告警" "$TMP/with-t.log" 'disables TLS certificate verification for local contract URL'

if "$NGINX_BIN" -p "$WITH" -c conf/nginx.conf >>"$TMP/with-t.log" 2>&1; then
  ok "带 allow-localhost-ca 的实例启动"
else
  bad "带 allow-localhost-ca 的实例启动" "$(head -3 "$TMP/with-t.log")"
fi

for _ in $(seq 1 50); do
  grep -q '\[local_gw\] contract loaded from url protocol=1 version=local-tls routes=1' \
    "$WITH/logs/error.log" 2>/dev/null && break
  req GET "http://127.0.0.1:$WITH_PORT/contract.json"
  sleep 0.1
done
log_has "  自签证书拉取成功，契约已加载" "$WITH/logs/error.log" \
  '[local_gw] contract loaded from url protocol=1 version=local-tls routes=1'

req GET "http://127.0.0.1:$WITH_PORT/contract.json"
expect_code "契约声明的路由放行 → 200（代理到本地 HTTPS 源）" 200
body_has "  响应来自本地 HTTPS 源" '"version":"local-tls"'

req GET "http://127.0.0.1:$WITH_PORT/nope"
expect_code "未声明路由 → 404" 404
head_has "  reason=route-not-declared" 'x-gateway-reason: route-not-declared'

section "2. 不带选项：自签证书必须失败并 fail-closed"
if "$NGINX_BIN" -p "$WITHOUT" -c conf/nginx.conf -t >"$TMP/without-t.log" 2>&1 \
  && "$NGINX_BIN" -p "$WITHOUT" -c conf/nginx.conf >>"$TMP/without-t.log" 2>&1; then
  ok "不带选项的实例启动（配置本身合法）"
else
  bad "不带选项的实例启动（配置本身合法）" "$(head -3 "$TMP/without-t.log")"
fi

for _ in $(seq 1 50); do
  grep -q '\[local_gw\] contract refresh failed' "$WITHOUT/logs/error.log" 2>/dev/null && break
  req GET "http://127.0.0.1:$WITHOUT_PORT/contract.json"
  sleep 0.1
done
log_has "  自签证书被拒绝（refresh failed）" "$WITHOUT/logs/error.log" \
  '[local_gw] contract refresh failed'

req GET "http://127.0.0.1:$WITHOUT_PORT/contract.json"
expect_code "契约从未加载成功 → 503 fail-closed" 503
head_has "  reason=contract-not-ready" 'x-gateway-reason: contract-not-ready'

section "3. allow-localhost-ca 的适用范围（nginx -t）"
check_bad_config() {
  local name="$1" url="$2" flag="$3" needle="$4"
  write_conf "$TMP/bad.conf" "$WITH_PORT" "logs/error.log" ""
  sed "s|https://127.0.0.1:$PY_PORT/contract.json ;|$url $flag;|" "$TMP/bad.conf" >"$TMP/bad-use.conf"
  if "$NGINX_BIN" -p "$WITH" -c "$TMP/bad-use.conf" -t >"$TMP/bad.log" 2>&1; then
    bad "$name" "nginx -t 竟然通过"
  else
    ok "$name"
  fi
  log_has "  $needle" "$TMP/bad.log" "$needle"
}

check_bad_config "非本地地址不允许 allow-localhost-ca" \
  "https://example.com/info" "allow-localhost-ca" 'allow-localhost-ca only applies'
check_bad_config "http:// 不允许 allow-localhost-ca" \
  "http://127.0.0.1:$PY_PORT/contract.json" "allow-localhost-ca" 'allow-localhost-ca only applies'
check_bad_config "未知选项被拒绝" \
  "https://127.0.0.1:$PY_PORT/contract.json" "bad-flag" 'unknown option "bad-flag"'

"$NGINX_BIN" -p "$WITH" -c conf/nginx.conf -s stop >/dev/null 2>&1
"$NGINX_BIN" -p "$WITHOUT" -c conf/nginx.conf -s stop >/dev/null 2>&1

printf '\n\033[1m结果：%d 通过，%d 失败\033[0m\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
