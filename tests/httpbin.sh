#!/usr/bin/env bash
# httpbin 演示实例验收：一个 nginx server 代理到 https://httpbin.org，
# 契约来自本地 JSON 文件（gateway_info_file），按协议 v1 验证网关行为。
#
# 前置：
#   ./scripts/build-module.sh   # 生成 nginx/modules/libmy_gateway.so
#   能访问 https://httpbin.org（测试会真实代理到公网 httpbin）
#
# 运行：
#   ./tests/httpbin.sh
set -uo pipefail

export NO_PROXY="127.0.0.1,localhost,::1"
export no_proxy="$NO_PROXY"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GATEWAY="http://127.0.0.1:18080"
NGINX_BIN="$ROOT/nginx/sbin/nginx"
MODULE="$ROOT/nginx/modules/libmy_gateway.so"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/httpbin-gw.XXXXXX")"
PREFIX="$TMP/prefix"
HTTPS_PREFIX="$TMP/https"

PASS=0
FAIL=0
CODE=""

ok() { printf '  \033[32mPASS\033[0m %s\n' "$1"; PASS=$((PASS + 1)); }
bad() { printf '  \033[31mFAIL\033[0m %s\n       %s\n' "$1" "$2"; FAIL=$((FAIL + 1)); }
section() { printf '\n\033[1m%s\033[0m\n' "$1"; }

cleanup() {
  for prefix in "$PREFIX" "$HTTPS_PREFIX"; do
    if [ -f "$prefix/logs/nginx.pid" ]; then
      "$NGINX_BIN" -p "$prefix" -c conf/nginx.conf -s stop >/dev/null 2>&1 || true
    fi
  done
  rm -rf "$TMP"
}
trap cleanup EXIT

req() {
  local method="$1" url="$2"
  shift 2
  CODE="$(curl -sS -m 20 -X "$method" -o "$TMP/body" -D "$TMP/head" -w '%{http_code}' "$@" "$url" 2>/dev/null)" || CODE="000"
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

body_lacks() {
  local name="$1" needle="$2"
  if grep -q -- "$needle" "$TMP/body"; then bad "$name" "响应体不该包含 '$needle'"; else ok "$name"; fi
}

head_has() {
  local name="$1" needle="$2"
  if grep -qi -- "$needle" "$TMP/head"; then ok "$name"; else bad "$name" "响应头缺少 '$needle'：$(tr -d '\r' <"$TMP/head" | head -8)"; fi
}

log_has() {
  local name="$1" needle="$2"
  if grep -q -- "$needle" "$PREFIX/logs/error.log" 2>/dev/null; then
    ok "$name"
  else
    bad "$name" "error.log 未找到 '$needle'"
  fi
}

section "0. 前置检查"
if [ ! -f "$MODULE" ]; then
  bad "模块存在" "缺少 $MODULE，请先运行 ./scripts/build-module.sh"
  printf '\n\033[1m结果：%d 通过，%d 失败\033[0m\n' "$PASS" "$FAIL"
  exit 1
fi
ok "模块存在"

if curl -sS -m 15 -o /dev/null -w '%{http_code}' https://httpbin.org/status/204 2>/dev/null | grep -q '^204$'; then
  ok "https://httpbin.org 可达"
else
  bad "https://httpbin.org 可达" "公网 httpbin 不可访问，无法运行本测试"
  printf '\n\033[1m结果：%d 通过，%d 失败\033[0m\n' "$PASS" "$FAIL"
  exit 1
fi

mkdir -p "$PREFIX/conf" "$PREFIX/logs" "$PREFIX/modules"
cp "$ROOT/deploy/httpbin/nginx.conf" "$PREFIX/conf/nginx.conf"
cp "$ROOT/deploy/httpbin/contract.json" "$PREFIX/conf/contract.json"
cp "$MODULE" "$PREFIX/modules/libmy_gateway.so"

if "$NGINX_BIN" -p "$PREFIX" -c conf/nginx.conf -t >"$TMP/nginx-t.log" 2>&1 \
  && "$NGINX_BIN" -p "$PREFIX" -c conf/nginx.conf >>"$TMP/nginx-t.log" 2>&1; then
  ok "独立实例启动（本地契约 gateway_info_file）"
else
  bad "独立实例启动（本地契约 gateway_info_file）" "$(head -5 "$TMP/nginx-t.log")"
  printf '\n\033[1m结果：%d 通过，%d 失败\033[0m\n' "$PASS" "$FAIL"
  exit 1
fi

for _ in $(seq 1 30); do
  grep -q 'contract loaded from file protocol=1 version=httpbin-demo-2 routes=19 unmatched=deny' \
    "$PREFIX/logs/error.log" 2>/dev/null && break
  req GET "$GATEWAY/get?name=probe"
  sleep 0.1
done
log_has "error_log 记录本地契约加载（19 条路由、deny）" \
  'contract loaded from file protocol=1 version=httpbin-demo-2 routes=19 unmatched=deny'

section "1. 放行：契约命中后真实代理到 httpbin"
req GET "$GATEWAY/get?name=ab"
expect_code "GET /get name=ab 放行" 200
body_has "  httpbin 回显 args.name" '"name": "ab"'
body_lacks "  不是网关错误 body" '"error"'

req GET "$GATEWAY/get?name=ab&age=30"
expect_code "GET /get 带可选 age 放行" 200
body_has "  httpbin 回显 args.age" '"age": "30"'

req GET "$GATEWAY/anything"
expect_code "GET /anything 放行" 200
body_has "  请求到达 httpbin /anything" '/anything'

section "2. Query 参数校验（协议 §5.1）"
req GET "$GATEWAY/get"
expect_code "缺少必填 name → 400" 400
head_has "  reason=missing-parameter" 'x-gateway-reason: missing-parameter'
head_has "  field=query.name" 'x-gateway-field: query.name'
head_has "  X-Gateway-Name" 'x-gateway-name: httpbin_gw'
body_has "  responses.missing_parameter 自定义 body" '"error":"missing-parameter"'
body_lacks "  请求未到 httpbin" '"args"'

req GET "$GATEWAY/get?name=a"
expect_code "name 长度不足 → 400" 400
head_has "  reason=invalid-parameter" 'x-gateway-reason: invalid-parameter'
head_has "  field=query.name" 'x-gateway-field: query.name'
body_has "  responses.invalid_parameter 自定义 body" '"error":"invalid-parameter"'

req GET "$GATEWAY/get?name=ab&age=abc"
expect_code "age 非 int → 400" 400
head_has "  field=query.age" 'x-gateway-field: query.age'
log_has "  日志 rule=int" 'field=query.age rule=int'

req GET "$GATEWAY/get?name=ab&age=121"
expect_code "age 超上限 → 400" 400
head_has "  field=query.age" 'x-gateway-field: query.age'
log_has "  日志 rule=max" 'field=query.age rule=max'

req GET "$GATEWAY/get?name=ab&extra=1"
expect_code "未声明 query 参数 → 400（unknown=reject）" 400
head_has "  field=query.extra" 'x-gateway-field: query.extra'
log_has "  日志 rule=unknown" 'field=query.extra rule=unknown'

req GET "$GATEWAY/get?name=ab&name=cd"
expect_code "同名 query 重复 → 400" 400
head_has "  field=query.name" 'x-gateway-field: query.name'
log_has "  日志 rule=duplicate" 'field=query.name rule=duplicate'

section "3. 方法与未声明路由（协议 §3.1 / §3.2）"
req POST "$GATEWAY/get?name=ab"
expect_code "POST /get → 405" 405
head_has "  reason=method-not-allowed" 'x-gateway-reason: method-not-allowed'
head_has "  Allow 只列出 GET" 'allow: GET'
body_has "  responses.method_not_allowed 自定义 body" '"error":"method-denied"'
body_lacks "  请求未到 httpbin" '"args"'

req GET "$GATEWAY/not-declared"
expect_code "未声明路由 → 404" 404
head_has "  reason=route-not-declared" 'x-gateway-reason: route-not-declared'
body_has "  responses.route_not_declared 自定义 body" '"error":"route-denied"'

section "4. Header 校验（协议 §5.3）"
req GET "$GATEWAY/headers"
expect_code "缺少 x-demo-token → 400" 400
head_has "  field=header.x-demo-token" 'x-gateway-field: header.x-demo-token'
log_has "  日志 rule=required" 'field=header.x-demo-token rule=required'

req GET "$GATEWAY/headers" -H 'x-demo-token: short'
expect_code "x-demo-token 长度不足 → 400" 400
head_has "  field=header.x-demo-token" 'x-gateway-field: header.x-demo-token'
log_has "  日志 rule=min_len" 'field=header.x-demo-token rule=min_len'

req GET "$GATEWAY/headers" -H 'x-demo-token: demo-token-123'
expect_code "合法 x-demo-token 放行" 200
body_has "  httpbin 回显该头" 'demo-token-123'

req GET "$GATEWAY/headers" -H 'X-Demo-Token: demo-token-123'
expect_code "header 名大小写不敏感" 200
body_has "  httpbin 回显该头" 'demo-token-123'

req GET "$GATEWAY/headers" -H 'x-demo-token: demo-token-123' -H 'x-demo-token: demo-token-456'
expect_code "同一必填 header 多行 → 400" 400
head_has "  field=header.x-demo-token" 'x-gateway-field: header.x-demo-token'
log_has "  日志 rule=duplicate" 'field=header.x-demo-token rule=duplicate'

section "5. Body 校验（协议 §5.2）"
req POST "$GATEWAY/post" -H 'Content-Type: application/json' --data '{}'
expect_code "缺少必填 body.username → 400" 400
head_has "  reason=missing-parameter" 'x-gateway-reason: missing-parameter'
head_has "  field=body.username" 'x-gateway-field: body.username'
body_lacks "  请求未到 httpbin" '"json"'

req POST "$GATEWAY/post" -H 'Content-Type: application/json' \
  --data '{"username":"alice","age":17}'
expect_code "age 低于 18 → 400" 400
head_has "  field=body.age" 'x-gateway-field: body.age'
log_has "  日志 rule=min" 'field=body.age rule=min'

req POST "$GATEWAY/post" -H 'Content-Type: application/json' \
  --data '{"username":"alice","age":20,"is_admin":true}'
expect_code "未声明 body 字段 → 400（unknown=reject）" 400
head_has "  field=body.is_admin" 'x-gateway-field: body.is_admin'
log_has "  日志 rule=unknown" 'field=body.is_admin rule=unknown'

req POST "$GATEWAY/post" -H 'Content-Type: text/plain' --data 'hello'
expect_code "非 JSON 媒体类型 → 415（responses 覆盖默认 400）" 415
head_has "  reason=unsupported-media-type" 'x-gateway-reason: unsupported-media-type'
body_has "  responses.unsupported_media_type 自定义 body" '"error":"unsupported-media-type"'
body_lacks "  请求未到 httpbin" '"json"'

req POST "$GATEWAY/post" -H 'Content-Type: application/json' \
  --data '{"username":"alice","age":20,"role":"user"}'
expect_code "合法 JSON body 放行" 200
body_has "  httpbin 回显 json.username" '"username": "alice"'
body_has "  httpbin 回显 json.role" '"role": "user"'

req POST "$GATEWAY/post" -H 'Content-Type: application/json' \
  --data '{"username":"alice","age":20,"role":"admin"}'
expect_code "required_if 成立但缺 note → 400" 400
head_has "  reason=missing-parameter" 'x-gateway-reason: missing-parameter'
head_has "  field=body.note" 'x-gateway-field: body.note'
log_has "  日志 rule=required_if" 'field=body.note rule=required_if'

req POST "$GATEWAY/post" -H 'Content-Type: application/json' \
  --data '{"username":"alice","age":20,"role":"ADMIN","note":"hi"}'
expect_code "enum ignore_case 命中 + required_if 满足 → 200" 200
body_has "  httpbin 回显 json.role" '"role": "ADMIN"'

req POST "$GATEWAY/post" -H 'Content-Type: application/json' \
  --data '{"username":"alice","age":20,"active":"yes"}'
expect_code "bool 只接受 true/false → 400" 400
head_has "  field=body.active" 'x-gateway-field: body.active'
log_has "  日志 rule=bool" 'field=body.active rule=bool'

req POST "$GATEWAY/post" -H 'Content-Type: application/json; charset=utf-8' \
  --data '{"username":"alice","age":20,"active":true}'
expect_code "Content-Type 带 charset 仍放行" 200
body_has "  httpbin 回显 json.active" '"active": true'

section "6. 路径参数与值规则（协议 §3.3 / §5.4）"
req GET "$GATEWAY/status/200"
expect_code "path code=200 放行（httpbin 返回 200）" 200

req GET "$GATEWAY/status/418"
expect_code "path code=418 超出 200..=299 → 400" 400
head_has "  field=path.code" 'x-gateway-field: path.code'
log_has "  日志 rule=max" 'field=path.code rule=max'
body_lacks "  未代理到 httpbin 的 418" 'teapot'

req GET "$GATEWAY/anything/abc"
expect_code "path slug=abc 匹配 regex 放行" 200
body_has "  httpbin 回显路径" '/anything/abc'

req GET "$GATEWAY/anything/AB"
expect_code "path slug=AB 不匹配 regex → 400" 400
head_has "  field=path.slug" 'x-gateway-field: path.slug'
log_has "  日志 rule=regex" 'field=path.slug rule=regex'

req GET "$GATEWAY/anything/wild/a/b"
expect_code "通配路径 {*rest} 跨段匹配 → 200" 200
body_has "  httpbin 回显完整通配路径" '/anything/wild/a/b'

req GET "$GATEWAY/anything/wild/"
expect_code "{*rest} 不匹配空串（/wild/）→ 404" 404

section "7. filters（SQLi / XSS，协议 §5.9）"
req GET "$GATEWAY/get?name=union+select+1"
expect_code "SQLi payload → 400" 400
head_has "  field=query.name" 'x-gateway-field: query.name'
body_has "  responses.invalid_parameter 自定义 body" '"error":"invalid-parameter"'
body_lacks "  请求未到 httpbin" '"args"'
log_has "  日志 filter=sqli" 'field=query.name rule=filter filter=sqli'

req GET "$GATEWAY/get?name=%3Cscript%3Ealert(1)%3C%2Fscript%3E"
expect_code "XSS payload → 400" 400
head_has "  field=query.name" 'x-gateway-field: query.name'
log_has "  日志 filter=xss" 'field=query.name rule=filter filter=xss'

req GET "$GATEWAY/get?name=hello"
expect_code "普通值不被过滤误杀" 200
body_has "  httpbin 收到普通值" '"name": "hello"'

section "8. 分组：继承 / 作用域 / regex / filters（协议 §3.4–3.5）"
req GET "$GATEWAY/anything/echo?trace=abc"
expect_code "组级 query trace 合法 → 200" 200
body_has "  httpbin 回显继承的 trace" '"trace": "abc"'

req GET "$GATEWAY/anything/echo"
expect_code "组级 query trace 可选，缺失仍放行" 200

req GET "$GATEWAY/anything/echo?trace=ab"
expect_code "组级 query 值规则（min_len）→ 400" 400
head_has "  field=query.trace" 'x-gateway-field: query.trace'
log_has "  日志 rule=min_len" 'field=query.trace rule=min_len'

req GET "$GATEWAY/anything/echo?trace=abc" -H 'x-group-token: 123456789'
expect_code "组级 header 值规则（max_len）→ 400" 400
head_has "  field=header.x-group-token" 'x-gateway-field: header.x-group-token'
log_has "  日志 rule=max_len" 'field=header.x-group-token rule=max_len'

req GET "$GATEWAY/anything/echo?trace=abc" -H 'x-group-token: 12345678'
expect_code "组级 header 合法值放行" 200

req POST "$GATEWAY/anything/echo?trace=abc"
expect_code "组 methods ∩ 路由 methods → 只允许 GET（405）" 405
head_has "  Allow 只列出 GET" 'allow: GET'
body_has "  responses.method_not_allowed 自定义 body" '"error":"method-denied"'

req GET "$GATEWAY/anything/items/123?trace=abc"
expect_code "嵌套组 path_params 合法 → 200" 200
body_has "  httpbin 回显路径" '/anything/items/123'

req GET "$GATEWAY/anything/items/0"
expect_code "嵌套组 path_params min 不满足 → 400" 400
head_has "  field=path.id" 'x-gateway-field: path.id'
log_has "  日志 rule=min" 'field=path.id rule=min'

req GET "$GATEWAY/anything/filter/echo?q=union+select+1"
expect_code "组级 filters 生效；路由 sqli:false 不能关闭 → 400" 400
head_has "  field=query.q" 'x-gateway-field: query.q'
log_has "  日志 filter=sqli" 'field=query.q rule=filter filter=sqli'

req GET "$GATEWAY/anything/filter/echo?q=%3Cscript%3Ealert(1)%3C%2Fscript%3E"
expect_code "组级 filters 的 XSS → 400" 400
log_has "  日志 filter=xss" 'field=query.q rule=filter filter=xss'

req GET "$GATEWAY/anything/filter/echo?q=hello"
expect_code "组级 filters 不误杀普通值" 200

req POST "$GATEWAY/anything/form/submit?trace=abc" \
  -H 'Content-Type: application/json' --data '{"source":"web"}'
expect_code "组级 body 规则继承（JSON）→ 200" 200
body_has "  httpbin 回显 body" '"source": "web"'

req POST "$GATEWAY/anything/form/submit" \
  -H 'Content-Type: application/json' --data '{"note":"hi"}'
expect_code "组级 body 必填缺失 → 400" 400
head_has "  field=body.source" 'x-gateway-field: body.source'
log_has "  日志 rule=required" 'field=body.source rule=required'

req POST "$GATEWAY/anything/form/submit" \
  -H 'Content-Type: application/json' --data '{"source":"mobile"}'
expect_code "组级 body enum 不匹配 → 400" 400
head_has "  field=body.source" 'x-gateway-field: body.source'
log_has "  日志 rule=values" 'field=body.source rule=values'

req POST "$GATEWAY/anything/form/submit" \
  -H 'Content-Type: application/json' --data '{"source":"web","extra":1}'
expect_code "组级 body unknown=reject → 400" 400
head_has "  field=body.extra" 'x-gateway-field: body.extra'
log_has "  日志 rule=unknown" 'field=body.extra rule=unknown'

req POST "$GATEWAY/anything/form/submit" \
  -H 'Content-Type: application/x-www-form-urlencoded' --data 'source=web'
expect_code "组级 body 也接受 form → 200" 200
body_has "  httpbin 回显 form.source" '"source": "web"'

req GET "$GATEWAY/anything/form/submit"
expect_code "组 methods ∩ 路由 methods → 只允许 POST（405）" 405
head_has "  Allow 只列出 POST" 'allow: POST'

req GET "$GATEWAY/anything/rx/alpha"
expect_code "组级 path_regex 命中 → 200" 200

req GET "$GATEWAY/anything/rx/gamma"
expect_code "子路由存在但组级 path_regex 不通过 → 404" 404
head_has "  reason=route-not-declared" 'x-gateway-reason: route-not-declared'

req GET "$GATEWAY/anything/admin/audit"
expect_code "分组声明路由放行" 200

req GET "$GATEWAY/anything/admin/other"
expect_code "分组 unmatched=forward：未声明子路径交给后端 → 200" 200
body_has "  httpbin 确实收到请求" '/anything/admin/other'

req GET "$GATEWAY/anything/admin/closed/other"
expect_code "更长的子分组 unmatched=deny 覆盖父级 → 404" 404
head_has "  reason=route-not-declared" 'x-gateway-reason: route-not-declared'

req GET "$GATEWAY/anything/admin/closed/ping"
expect_code "子分组声明路由放行" 200

section "9. priority 与路由级 path_regex（协议 §3.1.3 / §3.5）"
req GET "$GATEWAY/anything/priority/me"
expect_code "priority=10 的参数路由先匹配，id=me 校验失败 → 400" 400
head_has "  field=path.id" 'x-gateway-field: path.id'
log_has "  日志 rule=int" 'field=path.id rule=int'

req GET "$GATEWAY/anything/priority/42"
expect_code "priority 参数路由合法值 → 200" 200

req GET "$GATEWAY/anything/regex/abc"
expect_code "路由级 path_regex 命中 → 200" 200

req GET "$GATEWAY/anything/regex/ABC"
expect_code "路由级 path_regex 不通过 → 404（按未匹配处理）" 404
head_has "  reason=route-not-declared" 'x-gateway-reason: route-not-declared'

section "10. 扫描器路径按未声明路由拒绝（协议 §4）"
req GET "$GATEWAY/phpinfo.php"
expect_code "未声明路径 → 404" 404
head_has "  reason=route-not-declared" 'x-gateway-reason: route-not-declared'
body_lacks "  请求未到 httpbin" '"args"'
body_has "  responses.route_not_declared 自定义 body" '"error":"route-denied"'

section "11. 契约来源支持 HTTPS（https://httpbin.org/base64/...）"
mkdir -p "$HTTPS_PREFIX/conf" "$HTTPS_PREFIX/logs" "$HTTPS_PREFIX/modules"
cp "$MODULE" "$HTTPS_PREFIX/modules/libmy_gateway.so"

# 通过 httpbin /base64 把一份最小契约以 HTTPS 返回，验证 ureq 的 rustls 路径。
HTTPS_CONTRACT='{"protocol":1,"version":"https-demo","routes":[{"path":"/get","methods":["GET"]}]}'
HTTPS_B64="$(python3 -c 'import base64,sys,urllib.parse; print(urllib.parse.quote(base64.b64encode(sys.argv[1].encode()).decode(), safe=""))' "$HTTPS_CONTRACT")"

cat >"$HTTPS_PREFIX/conf/nginx.conf" <<CONF
worker_processes 1;
error_log logs/error.log notice;
pid logs/nginx.pid;
load_module modules/libmy_gateway.so;
events { worker_connections 64; }
http {
    access_log off;
    gateway_diagnostics on;
    gateway_name https_gw;
    gateway_info_url https_gw https://httpbin.org/base64/$HTTPS_B64;
    server {
        listen 127.0.0.1:18081;
        location / {
            enable_gateway_by https_gw;
            proxy_pass https://httpbin.org;
            proxy_ssl_server_name on;
            proxy_set_header Host httpbin.org;
            proxy_connect_timeout 5s;
            proxy_read_timeout 15s;
        }
    }
}
CONF

if "$NGINX_BIN" -p "$HTTPS_PREFIX" -c conf/nginx.conf -t >"$TMP/nginx-t-https.log" 2>&1 \
  && "$NGINX_BIN" -p "$HTTPS_PREFIX" -c conf/nginx.conf >>"$TMP/nginx-t-https.log" 2>&1; then
  ok "HTTPS 契约实例启动"
else
  bad "HTTPS 契约实例启动" "$(head -5 "$TMP/nginx-t-https.log")"
fi

HTTPS_GW="http://127.0.0.1:18081"
# 公网 HTTPS 偶发慢于 3s 全局超时；首次失败后 ContractManager 每 2s 重试一次。
for _ in $(seq 1 300); do
  grep -q '\[https_gw\] contract loaded from url protocol=1 version=https-demo routes=1 unmatched=deny' \
    "$HTTPS_PREFIX/logs/error.log" 2>/dev/null && break
  req GET "$HTTPS_GW/get?name=probe"
  sleep 0.1
done
if grep -q '\[https_gw\] contract loaded from url protocol=1 version=https-demo routes=1 unmatched=deny' \
  "$HTTPS_PREFIX/logs/error.log" 2>/dev/null; then
  ok "通过 HTTPS 拉取并加载契约（TLS 生效）"
else
  bad "通过 HTTPS 拉取并加载契约（TLS 生效）" \
    "$(grep -m1 'contract refresh failed' "$HTTPS_PREFIX/logs/error.log" 2>/dev/null)"
fi

req GET "$HTTPS_GW/get?name=ab"
expect_code "HTTPS 契约声明的路由放行 → 200" 200
body_has "  请求真实代理到 httpbin" '"name": "ab"'

req GET "$HTTPS_GW/post"
expect_code "HTTPS 契约未声明的路由 → 404" 404
head_has "  reason=route-not-declared" 'x-gateway-reason: route-not-declared'

"$NGINX_BIN" -p "$HTTPS_PREFIX" -c conf/nginx.conf -s stop >/dev/null 2>&1

printf '\n\033[1m结果：%d 通过，%d 失败\033[0m\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
