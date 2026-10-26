#!/usr/bin/env bash
# 阶段 1（含 1.5）curl 验收：直连后端 vs 经过网关，覆盖放行/探针/契约/多网关绑定。
# 前置：scripts/dev.sh up（100 项断言，全部通过才算过）
set -uo pipefail

# 本地测试永远不走代理
export NO_PROXY="127.0.0.1,localhost,::1"
export no_proxy="$NO_PROXY"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GATEWAY="${GATEWAY:-http://127.0.0.1:8080}"
BACKEND="${BACKEND:-http://127.0.0.1:9000}"
TMP="$(mktemp -d)"
trap 'if [ -f "$TMP/filegw/logs/nginx.pid" ]; then "$ROOT/nginx/sbin/nginx" -p "$TMP/filegw" -c conf/nginx.conf -s stop >/dev/null 2>&1; fi; rm -rf "$TMP"' EXIT

PASS=0
FAIL=0
CODE=""

ok() { printf '  \033[32mPASS\033[0m %s\n' "$1"; PASS=$((PASS + 1)); }
bad() { printf '  \033[31mFAIL\033[0m %s\n       %s\n' "$1" "$2"; FAIL=$((FAIL + 1)); }
section() { printf '\n\033[1m%s\033[0m\n' "$1"; }

# req METHOD URL [curl args...] -> $CODE，响应体在 $TMP/body，响应头在 $TMP/head
req() {
  local method="$1" url="$2"
  shift 2
  CODE="$(curl -sS -X "$method" -o "$TMP/body" -D "$TMP/head" -w '%{http_code}' "$@" "$url" 2>/dev/null)" || CODE="000"
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

head_lacks() {
  local name="$1" needle="$2"
  if grep -qi -- "$needle" "$TMP/head"; then bad "$name" "响应头不该包含 '$needle'"; else ok "$name"; fi
}

section "0. 连通性"
req GET "$BACKEND/api/health"
expect_code "后端直连可用" 200
if [ "$CODE" != "200" ]; then
  echo "  后端没起来，先执行 scripts/dev.sh up" >&2
  exit 1
fi
req GET "$GATEWAY/api/health"
expect_code "网关可用" 200

section "1. 正常业务请求（应放行，响应带 x-demo-backend 证明到了后端）"
req GET "$GATEWAY/api/health"
expect_code "GET /api/health" 200
body_has "  返回后端 JSON" '"status":"ok"'
head_has "  响应来自 demo_server" 'x-demo-backend: 1'

req GET "$GATEWAY/info"
expect_code "GET /info（固定契约）" 200
body_has "  契约含 /api/login" '/api/login'
body_has "  契约含动态路由" '/api/user/{id}'

req GET "$GATEWAY/api/products"
expect_code "GET /api/products" 200

req GET "$GATEWAY/api/user/42"
expect_code "GET /api/user/42" 200
body_has "  路径参数透传" '"id":"42"'

section "2. 登录接口（JSON 与表单两种 Body）"
req POST "$GATEWAY/api/login" -H 'Content-Type: application/json' \
  -d '{"username":"demo","password":"demo123","remember":true}'
expect_code "JSON 正确凭据" 200
body_has "  返回 token" 'demo-token-0001'

req POST "$GATEWAY/api/login" -H 'Content-Type: application/x-www-form-urlencoded' \
  -d 'username=demo&password=demo123&remember=false'
expect_code "表单正确凭据" 200

req POST "$GATEWAY/api/login" -H 'Content-Type: application/json' \
  -d '{"username":"demo","password":"wrong"}'
expect_code "错误密码" 401

req POST "$GATEWAY/api/login" -H 'Content-Type: application/json' -d '{"username":"demo"}'
expect_code "缺少 password" 400

section "3. 网关拦截（请求不应到达 demo_server）"
req GET "$GATEWAY/phpinfo.php"
expect_code "GET /phpinfo.php 被拦" 403
head_has "  标记来源" 'x-gateway-blocked: php-suffix'
body_lacks "  未返回后端内容" 'phpinfo() stub'

req GET "$GATEWAY/.env"
expect_code "GET /.env 被拦" 403

req GET "$GATEWAY/wp-admin/setup.php"
expect_code "GET /wp-admin/... 被拦" 403

req GET "$GATEWAY/__gateway/deny"
expect_code "GET /__gateway/deny 自定义状态码" 503
head_has "  标记来源" 'x-gateway-blocked: demo-deny'

section "4. 对照：证明拦截来自网关而不是后端"
req GET "$BACKEND/phpinfo.php"
expect_code "后端直连 /phpinfo.php 是 200" 200
body_has "  后端确实会返回内容" 'phpinfo() stub'

req GET "$GATEWAY/passthrough/phpinfo.php"
expect_code "未绑定网关时透传" 200
body_has "  透传拿到后端内容" 'phpinfo() stub'

section "5. 契约生效（未声明路由与方法校验）"
req GET "$GATEWAY/api/admin/users"
expect_code "未声明路由 404" 404
head_has "  标记原因" 'x-gateway-reason: route-not-declared'
head_has "  标记拒绝的网关" 'x-gateway-name: myrustwebdemo1'

req POST "$GATEWAY/api/health"
expect_code "方法不符 405" 405
head_has "  回写 Allow" 'allow: GET'
head_has "  标记拒绝的网关" 'x-gateway-name: myrustwebdemo1'

req DELETE "$GATEWAY/api/user/42"
expect_code "动态路由方法不符 405" 405

req GET "$GATEWAY/api/products/"
expect_code "尾斜杠严格不匹配 404" 404

# nginx 默认 merge_slashes on 会把 // 合并成 /，但上游收到的是原始路径；
# 判定必须用原始路径，否则会出现"网关放行、后端处理另一条路径"的不一致。
req GET "$GATEWAY//api/health"
expect_code "重复斜杠按原始路径判定（不合并）" 404
head_has "  标记原因" 'x-gateway-reason: route-not-declared'
head_has "  标记拒绝的网关" 'x-gateway-name: myrustwebdemo1'
body_lacks "  没有落到后端" 'not_found'

req GET "$GATEWAY/api//health"
expect_code "路径中间重复斜杠同样 404" 404

req GET "$GATEWAY/api/health?verbose=1"
expect_code "query 不参与路由匹配" 200

if grep -q 'gateway: GET //api/health (path=//api/health' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志里的判定路径是原始路径"
else
  bad "日志里的判定路径是原始路径" "未找到 'gateway: GET //api/health (path=//api/health'"
fi

req GET "$GATEWAY/passthrough/api/admin/users"
expect_code "未绑定网关时契约不生效" 200

section "6. 多网关：不同契约源（/info 与 /info_v2）"
req GET "$GATEWAY/info"
expect_code "两个网关都放行 /info" 200
body_has "  /info 响应仍是原契约" 'phase1-fixed'

req GET "$GATEWAY/info_v2"
expect_code "/info_v2 专属接口放行" 200
body_has "  响应来自 /info_v2 契约" '"contract":"forward-demo"'
body_has "  契约含 /api/metrics" '/api/metrics'

req GET "$GATEWAY/api/metrics"
expect_code "/info_v2 声明的接口放行" 200
head_has "  响应来自后端" 'x-demo-backend: 1'

req GET "$GATEWAY/api/reports/2026"
expect_code "/info_v2 动态路由放行" 200
body_has "  路径参数透传" '"year":"2026"'

req POST "$GATEWAY/info"
expect_code "任一网关拒绝即拒绝" 405
head_has "  标记第一个拒绝的网关" 'x-gateway-name: myrustwebdemo1'

section "7. 未声明路由策略（默认 deny / 显式 forward）"
# /info_v2 显式 unmatched: "forward"：契约没声明的路径交给后端
req GET "$GATEWAY/api/user/99"
expect_code "/info_v2 未声明路由转发到后端" 200
head_has "  响应来自后端" 'x-demo-backend: 1'
head_lacks "  不是网关拒绝" 'x-gateway-reason'
body_has "  后端返回真实数据" '"id":"99"'

req GET "$BACKEND/api/user/99"
expect_code "后端直连同一路径也是 200" 200

# 转发只影响"未声明"；已声明路由的方法校验仍然严格
req POST "$GATEWAY/api/metrics"
expect_code "/info_v2 已声明路由方法不符仍 405" 405
head_has "  回写 Allow" 'allow: GET'
head_has "  标记拒绝的网关" 'x-gateway-name: myrustwebdemo2'

# /info 没写 unmatched → 默认 deny，行为与以前一致
req GET "$GATEWAY/api/audit"
expect_code "/info 未声明仍是 404（默认 deny）" 404
head_has "  标记拒绝的网关是 myrustwebdemo1（指向 /info）" 'x-gateway-name: myrustwebdemo1'
head_has "  标记原因" 'x-gateway-reason: route-not-declared'

req GET "$GATEWAY/api/user/42"
expect_code "/info 已声明路由照常放行" 200

if grep -q 'gateway: forward GET /api/user/99 (route-not-declared, unmatched=forward) gateway=myrustwebdemo2' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录了转发行为"
else
  bad "日志记录了转发行为" "未找到 'gateway: forward GET /api/user/99 ... gateway=myrustwebdemo2'"
fi

if grep -q '\[myrustwebdemo2\] contract loaded protocol=1 version=phase1-forward routes=8 unmatched=forward' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录了 forward 契约的未声明策略"
else
  bad "日志记录了 forward 契约的未声明策略" "未找到 unmatched=forward"
fi

section "8. 日志"
if grep -q 'contract loaded protocol=1 version=phase1-fixed routes=8 unmatched=deny' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "error_log 记录了契约加载"
else
  bad "error_log 记录了契约加载" "未找到 'contract loaded protocol=1 version=phase1-fixed routes=8 unmatched=deny'"
fi

if grep -q '\[myrustwebdemo1\] contract loaded' "$ROOT/nginx/logs/error.log" 2>/dev/null \
  && grep -q '\[myrustwebdemo2\] contract loaded' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "两个网关各自拉取契约"
else
  bad "两个网关各自拉取契约" "error_log 缺少 myrustwebdemo1 / myrustwebdemo2 的 contract loaded"
fi

if grep -q '\[myrustwebdemo2\] contract loaded protocol=1 version=phase1-forward routes=8 unmatched=forward' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "demo2 拉到的是 /info_v2 的契约"
else
  bad "demo2 拉到的是 /info_v2 的契约" "未找到 '[myrustwebdemo2] contract loaded protocol=1 ... unmatched=forward'"
fi

if grep -q 'refresh started for 2 gateway(s)' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "error_log 记录刷新线程数量"
else
  bad "error_log 记录刷新线程数量" "未找到 'refresh started for 2 gateway(s)'"
fi

if grep -q 'gateway: GET /api/health' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "error_log 有插件访问日志"
else
  bad "error_log 有插件访问日志" "未找到 'gateway: GET /api/health'，检查 nginx/logs/error.log 与 error_log 级别"
fi

section "9. 配置校验（nginx -t）"
BASE_CONF="$ROOT/nginx/conf/nginx.conf"
nginx_t() { "$ROOT/nginx/sbin/nginx" -p "$ROOT/nginx" -c "$1" -t >"$TMP/nginx-t.log" 2>&1; }

sed 's/enable_gateway_by myrustwebdemo1;/enable_gateway_by nope;/' "$BASE_CONF" >"$TMP/t-unknown.conf"
if nginx_t "$TMP/t-unknown.conf"; then
  bad "未知网关名被拒绝" "nginx -t 竟然通过"
else
  ok "未知网关名被拒绝"
fi
if grep -q 'unknown gateway "nope"' "$TMP/nginx-t.log"; then
  ok "  错误信息包含网关名"
else
  bad "  错误信息包含网关名" "$(head -3 "$TMP/nginx-t.log")"
fi

sed '/gateway_info_url myrustwebdemo2/d' "$BASE_CONF" >"$TMP/t-no-url.conf"
if nginx_t "$TMP/t-no-url.conf"; then
  bad "gateway_name 缺少契约来源被拒绝" "nginx -t 竟然通过"
else
  ok "gateway_name 缺少契约来源被拒绝"
fi

sed 's/gateway_url_ttl  myrustwebdemo2 5;/gateway_url_ttl  myrustwebdemo2 0;/' "$BASE_CONF" >"$TMP/t-bad-ttl.conf"
if nginx_t "$TMP/t-bad-ttl.conf"; then
  bad "非法 gateway_url_ttl 被拒绝" "nginx -t 竟然通过"
else
  ok "非法 gateway_url_ttl 被拒绝"
fi

sed '/gateway_url_ttl  myrustwebdemo2/d' "$BASE_CONF" >"$TMP/t-default-ttl.conf"
if nginx_t "$TMP/t-default-ttl.conf"; then
  ok "省略 gateway_url_ttl 使用默认值"
else
  bad "省略 gateway_url_ttl 使用默认值" "$(head -3 "$TMP/nginx-t.log")"
fi

section "10. 契约文件模式（gateway_info_file，配置加载时读一次）"
FILE_PREFIX="$TMP/filegw"
FILE_GW="http://127.0.0.1:18099"
mkdir -p "$FILE_PREFIX/conf" "$FILE_PREFIX/logs"
file_nginx() { "$ROOT/nginx/sbin/nginx" -p "$FILE_PREFIX" -c conf/nginx.conf "$@"; }

cat >"$FILE_PREFIX/conf/contract.json" <<'JSON'
{
  "protocol": 1,
  "version": "file-mode-1",
  "routes": [{ "path": "/api/health", "methods": ["GET"] }]
}
JSON

cat >"$FILE_PREFIX/conf/nginx.conf" <<EOF
worker_processes 1;
error_log logs/error.log notice;
pid logs/nginx.pid;
load_module $ROOT/nginx/modules/libmy_gateway.so;
events { worker_connections 64; }
http {
    access_log off;
    gateway_name filegw;
    gateway_info_file filegw conf/contract.json;
    server {
        listen 127.0.0.1:18099;
        location / {
            enable_gateway_by filegw;
            proxy_pass http://127.0.0.1:9000;
        }
    }
}
EOF

if file_nginx -t >"$TMP/nginx-t-file.log" 2>&1; then
  ok "gateway_info_file 配置可加载（相对路径按 prefix 解析）"
else
  bad "gateway_info_file 配置可加载（相对路径按 prefix 解析）" "$(head -3 "$TMP/nginx-t-file.log")"
fi

if file_nginx >>"$TMP/nginx-t-file.log" 2>&1; then
  ok "文件模式实例启动"
else
  bad "文件模式实例启动" "$(head -3 "$TMP/nginx-t-file.log")"
fi

req GET "$FILE_GW/api/health"
expect_code "配置加载即生效：声明过的路由放行" 200
head_has "  请求确实到达后端" 'x-demo-backend: 1'

req GET "$FILE_GW/api/user/99"
expect_code "文件模式：未声明路由 404" 404
head_has "  标记原因" 'x-gateway-reason: route-not-declared'
head_has "  标记拒绝的网关" 'x-gateway-name: filegw'

req POST "$FILE_GW/api/health"
expect_code "文件模式：方法不符 405" 405
head_has "  Allow 头" 'allow: GET'

# 只改文件、不 reload：不应有任何变化（没有 TTL 轮询）
cat >"$FILE_PREFIX/conf/contract.json" <<'JSON'
{
  "protocol": 1,
  "version": "file-mode-2",
  "routes": [{ "path": "/api/user/{id}", "methods": ["GET"] }]
}
JSON
sleep 1
req GET "$FILE_GW/api/health"
expect_code "只改文件不 reload：旧契约继续生效" 200
req GET "$FILE_GW/api/user/99"
expect_code "只改文件不 reload：新路由未生效" 404

# reload 后读取新文件
file_nginx -s reload >/dev/null 2>&1
for _ in $(seq 1 50); do
  req GET "$FILE_GW/api/user/99"
  [ "$CODE" = "200" ] && break
  sleep 0.1
done
expect_code "reload 后新契约生效" 200
req GET "$FILE_GW/api/health"
expect_code "reload 后旧路由失效" 404

# 坏 JSON：与 nginx.conf 一样拒绝本次配置，旧 worker 继续服务
printf '{ "protocol": 1, "routes": [ ' >"$FILE_PREFIX/conf/contract.json"
if file_nginx -t >>"$TMP/nginx-t-file.log" 2>&1; then
  bad "坏 JSON 被 nginx -t 拒绝" "nginx -t 竟然通过"
else
  ok "坏 JSON 被 nginx -t 拒绝"
fi
if grep -q 'contract file .* rejected' "$TMP/nginx-t-file.log"; then
  ok "  错误信息说明契约被拒绝"
else
  bad "  错误信息说明契约被拒绝" "$(grep -m1 gateway "$TMP/nginx-t-file.log")"
fi
file_nginx -s reload >/dev/null 2>&1
sleep 0.5
req GET "$FILE_GW/api/user/99"
expect_code "reload 失败：旧 worker 继续用旧契约" 200

# 文件缺失：同样拒绝配置
rm -f "$FILE_PREFIX/conf/contract.json"
if file_nginx -t >>"$TMP/nginx-t-file.log" 2>&1; then
  bad "契约文件缺失被拒绝" "nginx -t 竟然通过"
else
  ok "契约文件缺失被拒绝"
fi
if grep -q 'cannot read contract file' "$TMP/nginx-t-file.log"; then
  ok "  错误信息说明读不到文件"
else
  bad "  错误信息说明读不到文件" "$(grep -m1 gateway "$TMP/nginx-t-file.log")"
fi

file_nginx -s stop >/dev/null 2>&1

section "11. 契约来源与指令互斥（nginx -t）"
sed "s|gateway_info_url myrustwebdemo2 http://127.0.0.1:9000/info_v2;|&\n    gateway_info_file myrustwebdemo2 $TMP/contract-both.json;|" \
  "$BASE_CONF" >"$TMP/t-both.conf"
if nginx_t "$TMP/t-both.conf"; then
  bad "url 与 file 同时指定被拒绝" "nginx -t 竟然通过"
else
  ok "url 与 file 同时指定被拒绝"
fi
if grep -q 'already has a contract source' "$TMP/nginx-t.log"; then
  ok "  错误信息说明来源互斥"
else
  bad "  错误信息说明来源互斥" "$(head -3 "$TMP/nginx-t.log")"
fi

# 文件来源不允许 gateway_url_ttl（它只管 URL 刷新）
cat >"$FILE_PREFIX/conf/contract.json" <<'JSON'
{ "protocol": 1, "routes": [{ "path": "/api/health", "methods": ["GET"] }] }
JSON
sed 's|gateway_info_file filegw conf/contract.json;|&\n    gateway_url_ttl filegw 2;|' \
  "$FILE_PREFIX/conf/nginx.conf" >"$FILE_PREFIX/conf/ttl.conf"
if file_nginx -c conf/ttl.conf -t >"$TMP/nginx-t-ttl.log" 2>&1; then
  bad "文件来源写 gateway_url_ttl 被拒绝" "nginx -t 竟然通过"
else
  ok "文件来源写 gateway_url_ttl 被拒绝"
fi
if grep -q 'only applies to' "$TMP/nginx-t-ttl.log"; then
  ok "  错误信息说明 TTL 只对 URL 生效"
else
  bad "  错误信息说明 TTL 只对 URL 生效" "$(head -3 "$TMP/nginx-t-ttl.log")"
fi

printf '\n\033[1m结果：%d 通过，%d 失败\033[0m\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
