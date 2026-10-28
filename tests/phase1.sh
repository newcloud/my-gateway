#!/usr/bin/env bash
# 阶段 1（含 1.5）curl 验收：直连后端 vs 经过网关，覆盖放行/契约/多网关绑定。
# 前置：scripts/dev.sh up（524 项断言，全部通过才算过）
set -uo pipefail

# 本地测试永远不走代理
export NO_PROXY="127.0.0.1,localhost,::1"
export no_proxy="$NO_PROXY"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GATEWAY="${GATEWAY:-http://127.0.0.1:8080}"
BACKEND="${BACKEND:-http://127.0.0.1:9000}"
TMP="$(mktemp -d)"
trap 'for prefix in "$TMP/filegw" "$TMP/urlsrc" "$TMP/diagoff"; do if [ -f "$prefix/logs/nginx.pid" ]; then "$ROOT/nginx/sbin/nginx" -p "$prefix" -c conf/nginx.conf -s stop >/dev/null 2>&1; fi; done; rm -rf "$TMP"' EXIT

# 只保留「最近一次启动」之后的 error_log：dev.sh restart 是追加写，上一次运行留下的
# `filter=sqli`、`rule=min` 之类日志会让断言假通过（httpbin.sh 用全新 prefix 才暴露过）。
# 保留最近一次启动标记之后的全部行（契约加载 / manager 启动），并把原文件备份一份。
LOG_FILE="$ROOT/nginx/logs/error.log"
if [ -f "$LOG_FILE" ]; then
  start="$(grep -n 'using the "epoll" event method' "$LOG_FILE" 2>/dev/null | tail -1 | cut -d: -f1)"
  [ -n "${start:-}" ] || start=1
  cp "$LOG_FILE" "$LOG_FILE.before-phase1" 2>/dev/null || true
  tail -n +"$start" "$LOG_FILE" >"$TMP/error.log.trim" && cat "$TMP/error.log.trim" >"$LOG_FILE"
fi

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

section "3. 未声明路径（含扫描器路径）按契约拒绝"
# 不再有内置探针：这些路径只是未声明，按 route-not-declared 404 处理。
req GET "$GATEWAY/phpinfo.php"
expect_code "GET /phpinfo.php 未声明 → 404" 404
head_has "  标记原因" 'x-gateway-reason: route-not-declared'
body_lacks "  未返回后端内容" 'phpinfo() stub'

req GET "$GATEWAY/.env"
expect_code "GET /.env 未声明 → 404" 404
head_has "  标记原因" 'x-gateway-reason: route-not-declared'

req GET "$GATEWAY/wp-admin/setup.php"
expect_code "GET /wp-admin/... 未声明 → 404" 404

req GET "$GATEWAY/wp-includes/load"
expect_code "GET /wp-includes/... 未声明 → 404" 404
head_has "  标记原因" 'x-gateway-reason: route-not-declared'

req GET "$GATEWAY/a/.env"
expect_code "非根路径的 .env 未声明 → 404" 404
head_has "  标记原因" 'x-gateway-reason: route-not-declared'

req GET "$GATEWAY/evil.env"
expect_code ".env 后缀未声明 → 404" 404

req GET "$GATEWAY/.github/workflows"
expect_code "GET /.github/... 未声明 → 404" 404
head_has "  标记原因" 'x-gateway-reason: route-not-declared'

req GET "$GATEWAY/__gateway/deny"
expect_code "GET /__gateway/deny 未声明 → 404" 404
head_has "  标记原因" 'x-gateway-reason: route-not-declared'

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

section "5.5 Header 存在性校验（专用路由，不混入值规则）"
# 契约 /info 的 /api/header-check 只声明：
#   x-demo-token: true                → 必填
#   x-demo-note:  { required: false } → 显式可选
# 这条路由不涉及任何值规则（值规则在 5.6 的专用路由验证）。

# 缺必填头：400，且请求不应到达后端
req GET "$GATEWAY/api/header-check"
expect_code "缺必填头 → 400" 400
head_has "  标记原因" 'x-gateway-reason: missing-parameter'
head_has "  指向缺失字段" 'x-gateway-field: header.x-demo-token'
head_has "  标记拒绝的网关" 'x-gateway-name: myrustwebdemo1'
head_lacks "  未落到后端" 'x-demo-backend:'

# 头名大小写不敏感：契约写小写，请求用大写也应放行
req GET "$GATEWAY/api/header-check" -H 'X-Demo-Token: abc123'
expect_code "带必填头放行（头名大小写不敏感）" 200
head_has "  响应来自 demo_server" 'x-demo-backend: 1'

# 显式可选头：存在与否都不影响
req GET "$GATEWAY/api/header-check" -H 'x-demo-token: abc123' -H 'x-demo-note: hello'
expect_code "显式可选头存在仍放行" 200

# 契约没声明的头：忽略
req GET "$GATEWAY/api/header-check" -H 'x-demo-token: abc123' -H 'x-undeclared: whatever'
expect_code "未声明的头不影响判定" 200

# 同一个头出现多行：400 invalid-parameter（必填头）
req GET "$GATEWAY/api/header-check" -H 'x-demo-token: one' -H 'x-demo-token: two'
expect_code "同一必填头多行 → 400" 400
head_has "  标记原因" 'x-gateway-reason: invalid-parameter'
head_has "  指向重复字段" 'x-gateway-field: header.x-demo-token'
head_lacks "  未落到后端" 'x-demo-backend:'

# 显式可选的头同样不允许重复行
req GET "$GATEWAY/api/header-check" -H 'x-demo-token: abc123' \
  -H 'x-demo-note: one' -H 'x-demo-note: two'
expect_code "同一可选头多行 → 400" 400
head_has "  指向重复字段" 'x-gateway-field: header.x-demo-note'

if grep -q 'reason=missing-parameter field=header.x-demo-token rule=required' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录 Header 缺失（rule=required）"
else
  bad "日志记录 Header 缺失（rule=required）" "未找到 'reason=missing-parameter field=header.x-demo-token rule=required'"
fi

if grep -q 'reason=invalid-parameter field=header.x-demo-token rule=duplicate' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录 Header 重复（rule=duplicate）"
else
  bad "日志记录 Header 重复（rule=duplicate）" "未找到 'reason=invalid-parameter field=header.x-demo-token rule=duplicate'"
fi

section "5.6 Header 值规则（type / 长度 / 范围 / 枚举 / regex）"
# 契约 /info 的 /api/header-value-check 声明：
#   x-demo-token: char, required, 2..=8
#   x-demo-count: int, 1..=99
#   x-demo-mode:  enum(alpha,beta), ignore_case
#   x-demo-code:  regex [A-Z]{2}[0-9]{2}（全匹配）

# 全部合法（枚举大小写不敏感）→ 放行，且值原样到达后端
req GET "$GATEWAY/api/header-value-check" \
  -H 'x-demo-token: tok' -H 'x-demo-count: 42' \
  -H 'x-demo-mode: ALPHA' -H 'x-demo-code: AB12'
expect_code "全部值合法 → 200" 200
head_has "  响应来自 demo_server" 'x-demo-backend: 1'
body_has "  后端收到 token" '"token":"tok"'
body_has "  枚举值大小写不敏感（原样转发）" '"mode":"ALPHA"'
body_has "  后端收到 int" '"count":"42"'

# 值规则是可选的：没出现的头直接跳过
req GET "$GATEWAY/api/header-value-check" -H 'x-demo-token: tok'
expect_code "未出现的可选值规则跳过校验" 200
head_has "  响应来自 demo_server" 'x-demo-backend: 1'

# 同契约里，必填缺失优先于其他字段的值错误
req GET "$GATEWAY/api/header-value-check" -H 'x-demo-count: 0'
expect_code "必填缺失优先于值错误 → 400" 400
head_has "  标记原因" 'x-gateway-reason: missing-parameter'
head_has "  指向必填字段" 'x-gateway-field: header.x-demo-token'
head_lacks "  未落到后端" 'x-demo-backend:'

# char 长度（Unicode 码点）
req GET "$GATEWAY/api/header-value-check" -H 'x-demo-token: a'
expect_code "char 短于 min_len → 400" 400
head_has "  标记原因" 'x-gateway-reason: invalid-parameter'
head_has "  指向字段" 'x-gateway-field: header.x-demo-token'
head_lacks "  未落到后端" 'x-demo-backend:'

req GET "$GATEWAY/api/header-value-check" -H 'x-demo-token: abcdefghi'
expect_code "char 长于 max_len → 400" 400
head_has "  指向字段" 'x-gateway-field: header.x-demo-token'

# int 语法与范围
req GET "$GATEWAY/api/header-value-check" -H 'x-demo-token: tok' -H 'x-demo-count: abc'
expect_code "int 语法不符 → 400" 400
head_has "  指向字段" 'x-gateway-field: header.x-demo-count'

req GET "$GATEWAY/api/header-value-check" -H 'x-demo-token: tok' -H 'x-demo-count: 007'
expect_code "int 拒绝前导零 → 400" 400
head_has "  指向字段" 'x-gateway-field: header.x-demo-count'

req GET "$GATEWAY/api/header-value-check" -H 'x-demo-token: tok' -H 'x-demo-count: 0'
expect_code "int 低于 min → 400" 400
head_has "  指向字段" 'x-gateway-field: header.x-demo-count'

req GET "$GATEWAY/api/header-value-check" -H 'x-demo-token: tok' -H 'x-demo-count: 100'
expect_code "int 高于 max → 400" 400
head_has "  指向字段" 'x-gateway-field: header.x-demo-count'

# enum：ignore_case=true 只放宽大小写，不放宽取值
req GET "$GATEWAY/api/header-value-check" -H 'x-demo-token: tok' -H 'x-demo-mode: gamma'
expect_code "enum 不在允许值 → 400" 400
head_has "  标记原因" 'x-gateway-reason: invalid-parameter'
head_has "  指向字段" 'x-gateway-field: header.x-demo-mode'
head_lacks "  未落到后端" 'x-demo-backend:'

# regex 是全匹配：前缀/后缀多余字符都不行
req GET "$GATEWAY/api/header-value-check" -H 'x-demo-token: tok' -H 'x-demo-code: AB12x'
expect_code "regex 尾部多余字符 → 400" 400
head_has "  指向字段" 'x-gateway-field: header.x-demo-code'
head_lacks "  未落到后端" 'x-demo-backend:'

req GET "$GATEWAY/api/header-value-check" -H 'x-demo-token: tok' -H 'x-demo-code: xAB12'
expect_code "regex 头部多余字符 → 400" 400
head_has "  指向字段" 'x-gateway-field: header.x-demo-code'

# 重复行优先于值校验
req GET "$GATEWAY/api/header-value-check" -H 'x-demo-token: tok' \
  -H 'x-demo-count: 1' -H 'x-demo-count: 2'
expect_code "值规则字段多行 → 400" 400
head_has "  标记原因" 'x-gateway-reason: invalid-parameter'
head_has "  指向重复字段" 'x-gateway-field: header.x-demo-count'

# 未声明的头仍然忽略
req GET "$GATEWAY/api/header-value-check" -H 'x-demo-token: tok' -H 'x-undeclared: zzz'
expect_code "未声明的头不影响值校验" 200

if grep -q 'field=header.x-demo-count rule=int' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录 int 语法失败（rule=int）"
else
  bad "日志记录 int 语法失败（rule=int）" "未找到 'field=header.x-demo-count rule=int'"
fi

if grep -q 'field=header.x-demo-count rule=min' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录 int 下界（rule=min）"
else
  bad "日志记录 int 下界（rule=min）" "未找到 'field=header.x-demo-count rule=min'"
fi

if grep -q 'field=header.x-demo-count rule=max' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录 int 上界（rule=max）"
else
  bad "日志记录 int 上界（rule=max）" "未找到 'field=header.x-demo-count rule=max'"
fi

if grep -q 'field=header.x-demo-token rule=min_len' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录 char 最小长度（rule=min_len）"
else
  bad "日志记录 char 最小长度（rule=min_len）" "未找到 'field=header.x-demo-token rule=min_len'"
fi

if grep -q 'field=header.x-demo-token rule=max_len' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录 char 最大长度（rule=max_len）"
else
  bad "日志记录 char 最大长度（rule=max_len）" "未找到 'field=header.x-demo-token rule=max_len'"
fi

if grep -q 'field=header.x-demo-mode rule=values' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录枚举不匹配（rule=values）"
else
  bad "日志记录枚举不匹配（rule=values）" "未找到 'field=header.x-demo-mode rule=values'"
fi

if grep -q 'field=header.x-demo-code rule=regex' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录正则不匹配（rule=regex）"
else
  bad "日志记录正则不匹配（rule=regex）" "未找到 'field=header.x-demo-code rule=regex'"
fi

if grep -q 'field=header.x-demo-count rule=duplicate' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录值规则字段重复（rule=duplicate）"
else
  bad "日志记录值规则字段重复（rule=duplicate）" "未找到 'field=header.x-demo-count rule=duplicate'"
fi

section "5.7 Header 条件必填（required_if）"
# 契约 /info 的 /api/header-required-if 声明：
#   x-demo-mode:   enum(secure, open)
#   x-demo-auth:   required_if { field: x-demo-mode, equals: secure } + char min_len 2
#   x-demo-note:   { required: false }
#   x-demo-legacy: required_if "x-demo-note"（字符串形式）+ int

# 条件不成立，且目标头未出现 → 放行
req GET "$GATEWAY/api/header-required-if"
expect_code "条件不成立且目标缺失 → 200" 200
head_has "  响应来自 demo_server" 'x-demo-backend: 1'

req GET "$GATEWAY/api/header-required-if" -H 'x-demo-mode: open'
expect_code "mode=open 不触发条件必填" 200

# 对象形式：值相等才成立
req GET "$GATEWAY/api/header-required-if" -H 'x-demo-mode: secure'
expect_code "mode=secure 缺 auth → 400" 400
head_has "  标记原因" 'x-gateway-reason: missing-parameter'
head_has "  指向条件必填字段" 'x-gateway-field: header.x-demo-auth'
head_lacks "  未落到后端" 'x-demo-backend:'

req GET "$GATEWAY/api/header-required-if" -H 'x-demo-mode: secure' -H 'x-demo-auth: ok'
expect_code "条件成立且目标存在 → 200" 200
head_has "  响应来自 demo_server" 'x-demo-backend: 1'
body_has "  后端收到 auth" '"auth":"ok"'

# 目标存在后仍按自身值规则校验（条件成立/不成立都一样）
req GET "$GATEWAY/api/header-required-if" -H 'x-demo-mode: secure' -H 'x-demo-auth: a'
expect_code "条件成立但值太短 → 400" 400
head_has "  指向字段" 'x-gateway-field: header.x-demo-auth'

req GET "$GATEWAY/api/header-required-if" -H 'x-demo-mode: open' -H 'x-demo-auth: a'
expect_code "条件不成立但出现值仍校验 → 400" 400
head_has "  指向字段" 'x-gateway-field: header.x-demo-auth'

# 字符串形式：被引用头出现即成立，空值也算出现
req GET "$GATEWAY/api/header-required-if" -H 'x-demo-note: 1'
expect_code "note 出现缺 legacy → 400" 400
head_has "  标记原因" 'x-gateway-reason: missing-parameter'
head_has "  指向条件必填字段" 'x-gateway-field: header.x-demo-legacy'
head_lacks "  未落到后端" 'x-demo-backend:'

req GET "$GATEWAY/api/header-required-if" -H 'x-demo-note;'
expect_code "note 为空也算出现 → 400" 400
head_has "  指向条件必填字段" 'x-gateway-field: header.x-demo-legacy'

req GET "$GATEWAY/api/header-required-if" -H 'x-demo-mode: open' -H 'x-demo-legacy: 5'
expect_code "note 缺失时 legacy 可选" 200
head_has "  响应来自 demo_server" 'x-demo-backend: 1'

req GET "$GATEWAY/api/header-required-if" -H 'x-demo-note: 1' -H 'x-demo-legacy: abc'
expect_code "legacy 出现但值非法 → 400" 400
head_has "  指向字段" 'x-gateway-field: header.x-demo-legacy'

# 两个条件都成立且都缺失：按契约顺序报 auth（写在 legacy 之前）
req GET "$GATEWAY/api/header-required-if" -H 'x-demo-mode: secure' -H 'x-demo-note: 1'
expect_code "多个条件必填缺失按契约顺序" 400
head_has "  先报 auth" 'x-gateway-field: header.x-demo-auth'

if grep -q 'field=header.x-demo-auth rule=required_if' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录对象形式条件必填（rule=required_if）"
else
  bad "日志记录对象形式条件必填（rule=required_if）" "未找到 'field=header.x-demo-auth rule=required_if'"
fi

if grep -q 'field=header.x-demo-legacy rule=required_if' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录字符串形式条件必填（rule=required_if）"
else
  bad "日志记录字符串形式条件必填（rule=required_if）" "未找到 'field=header.x-demo-legacy rule=required_if'"
fi

if grep -q 'field=header.x-demo-auth rule=min_len' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录条件字段的值校验（rule=min_len）"
else
  bad "日志记录条件字段的值校验（rule=min_len）" "未找到 'field=header.x-demo-auth rule=min_len'"
fi

if grep -q 'field=header.x-demo-legacy rule=int' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录条件字段的 int 校验（rule=int）"
else
  bad "日志记录条件字段的 int 校验（rule=int）" "未找到 'field=header.x-demo-legacy rule=int'"
fi

section "5.8 Query 参数校验"
# 契约 /info 的 /api/query-check：
#   page  int, required, 1..99
#   mode  enum(secure, open)
#   token required_if mode==secure, char min_len 2
#   kw    char max_len 8
#   flag  bool
#   code  regex [A-Z]{2}[0-9]{2}
#   unknown: ignore
# /api/query-strict：page int min 1，unknown: reject

req GET "$GATEWAY/api/query-check?page=5"
expect_code "合法 query → 200" 200
head_has "  响应来自 demo_server" 'x-demo-backend: 1'
body_has "  后端收到原始 query" '"query":"page=5"'

req GET "$GATEWAY/api/query-check"
expect_code "缺必填 query → 400" 400
head_has "  标记原因" 'x-gateway-reason: missing-parameter'
head_has "  指向字段" 'x-gateway-field: query.page'
head_lacks "  未落到后端" 'x-demo-backend:'

req GET "$GATEWAY/api/query-check?page=0"
expect_code "query int 低于 min → 400" 400
head_has "  指向字段" 'x-gateway-field: query.page'

req GET "$GATEWAY/api/query-check?page=abc"
expect_code "query int 语法不符 → 400" 400
head_has "  指向字段" 'x-gateway-field: query.page'

req GET "$GATEWAY/api/query-check?page=1&mode=secure"
expect_code "query 条件必填缺失 → 400" 400
head_has "  标记原因" 'x-gateway-reason: missing-parameter'
head_has "  指向条件字段" 'x-gateway-field: query.token'

req GET "$GATEWAY/api/query-check?page=1&mode=secure&token=a"
expect_code "query 条件字段值太短 → 400" 400
head_has "  指向字段" 'x-gateway-field: query.token'

req GET "$GATEWAY/api/query-check?page=1&mode=secure&token=ok"
expect_code "条件成立且字段合法 → 200" 200

req GET "$GATEWAY/api/query-check?page=1&mode=open"
expect_code "条件不成立时 token 可选 → 200" 200

req GET "$GATEWAY/api/query-check?page=1&kw=%E4%BD%A0%E5%A5%BD"
expect_code "query 百分号解码后长度合法 → 200" 200
body_has "  后端收到原始编码" '"query":"page=1&kw=%E4%BD%A0%E5%A5%BD"'

req GET "$GATEWAY/api/query-check?page=1&flag=TRUE&code=AB12"
expect_code "query bool + regex 合法 → 200" 200

req GET "$GATEWAY/api/query-check?page=1&flag=yes"
expect_code "query bool 非法 → 400" 400
head_has "  指向字段" 'x-gateway-field: query.flag'

req GET "$GATEWAY/api/query-check?page=1&code=AB12x"
expect_code "query regex 全匹配失败 → 400" 400
head_has "  指向字段" 'x-gateway-field: query.code'

req GET "$GATEWAY/api/query-check?page=1&extra=1"
expect_code "query 未声明参数默认忽略 → 200" 200

req GET "$GATEWAY/api/query-strict?page=1&extra=1"
expect_code "query unknown=reject → 400" 400
head_has "  标记原因" 'x-gateway-reason: invalid-parameter'
head_has "  指向未声明字段" 'x-gateway-field: query.extra'
head_lacks "  未落到后端" 'x-demo-backend:'

req GET "$GATEWAY/api/query-check?page=1&page=2"
expect_code "query 同名参数重复 → 400" 400
head_has "  指向重复字段" 'x-gateway-field: query.page'

req GET "$GATEWAY/api/query-check?page=%ZZ"
expect_code "query 非法百分号编码 → 400" 400
head_has "  标记原因" 'x-gateway-reason: invalid-parameter'

if grep -q 'field=query.page rule=required' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录 query 必填缺失（rule=required）"
else
  bad "日志记录 query 必填缺失（rule=required）" "未找到 'field=query.page rule=required'"
fi

if grep -q 'field=query.token rule=required_if' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录 query 条件必填（rule=required_if）"
else
  bad "日志记录 query 条件必填（rule=required_if）" "未找到 'field=query.token rule=required_if'"
fi

if grep -q 'field=query.extra rule=unknown' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录 query 未声明字段（rule=unknown）"
else
  bad "日志记录 query 未声明字段（rule=unknown）" "未找到 'field=query.extra rule=unknown'"
fi

section "5.9 Body 参数校验"
# 契约 /info 的 /api/body-check（JSON 与 form 都接受，默认 unknown=reject）：
#   sku  char, required, max_len 8
#   qty  int, required, 1..99
#   note char, max_len 16

req POST "$GATEWAY/api/body-check" -H 'Content-Type: application/json' \
  --data-binary '{"sku":"A-1","qty":2}'
expect_code "合法 JSON body → 200" 200
head_has "  响应来自 demo_server" 'x-demo-backend: 1'
body_has "  后端收到原始 JSON" '\\"qty\\":2'

req POST "$GATEWAY/api/body-check" -H 'Content-Type: application/json' \
  --data-binary '{"sku":"A-1"}'
expect_code "body 缺必填字段 → 400" 400
head_has "  标记原因" 'x-gateway-reason: missing-parameter'
head_has "  指向字段" 'x-gateway-field: body.qty'
head_lacks "  未落到后端" 'x-demo-backend:'

req POST "$GATEWAY/api/body-check" -H 'Content-Type: application/json' \
  --data-binary '{"sku":"A-1","qty":0}'
expect_code "body int 低于 min → 400" 400
head_has "  指向字段" 'x-gateway-field: body.qty'

req POST "$GATEWAY/api/body-check" -H 'Content-Type: application/json' \
  --data-binary '{"sku":"A-1","qty":"abc"}'
expect_code "body int 语法不符 → 400" 400
head_has "  指向字段" 'x-gateway-field: body.qty'

req POST "$GATEWAY/api/body-check" -H 'Content-Type: application/json' \
  --data-binary '{"sku":"A-1","qty":2.0}'
expect_code "body JSON 浮点不能当 int → 400" 400
head_has "  指向字段" 'x-gateway-field: body.qty'

req POST "$GATEWAY/api/body-check" -H 'Content-Type: application/json' \
  --data-binary '{"sku":"123456789","qty":2}'
expect_code "body char 超长 → 400" 400
head_has "  指向字段" 'x-gateway-field: body.sku'

req POST "$GATEWAY/api/body-check" -H 'Content-Type: application/json' \
  --data-binary '{"sku":"A-1","qty":2,"is_admin":true}'
expect_code "body 未声明字段默认拒绝 → 400" 400
head_has "  标记原因" 'x-gateway-reason: invalid-parameter'
head_has "  指向未声明字段" 'x-gateway-field: body.is_admin'
head_lacks "  未落到后端" 'x-demo-backend:'

# null 对“声明字段”等于未提供，但“未声明字段”仍然是出现：否则 unknown=reject 会被绕过。
req POST "$GATEWAY/api/body-check" -H 'Content-Type: application/json' \
  --data-binary '{"sku":"A-1","qty":2,"is_admin":null}'
expect_code "body 未声明字段为 null 仍拒绝 → 400" 400
head_has "  指向未声明字段" 'x-gateway-field: body.is_admin'
head_lacks "  未落到后端" 'x-demo-backend:'

req POST "$GATEWAY/api/body-check" -H 'Content-Type: application/x-www-form-urlencoded' \
  --data-binary 'sku=A-1&qty=2&note=ok'
expect_code "合法 form body → 200" 200
body_has "  后端收到原始 form" 'sku=A-1&qty=2&note=ok'

req POST "$GATEWAY/api/body-check" -H 'Content-Type: application/x-www-form-urlencoded' \
  --data-binary 'sku=A-1&qty=2&qty=3'
expect_code "form 同名键重复 → 400" 400
head_has "  指向重复字段" 'x-gateway-field: body.qty'

req POST "$GATEWAY/api/body-check" -H 'Content-Type:' --data-binary '{"sku":"A-1","qty":2}'
expect_code "非空 body 缺 Content-Type → 415" 415
head_has "  标记原因" 'x-gateway-reason: unsupported-media-type'

req POST "$GATEWAY/api/body-check" -H 'Content-Type: text/plain' --data-binary 'x'
expect_code "不支持的 Content-Type → 415" 415
head_has "  标记原因" 'x-gateway-reason: unsupported-media-type'

req POST "$GATEWAY/api/body-check" -H 'Content-Type: application/json; charset=iso-8859-1' \
  --data-binary '{"sku":"A-1","qty":2}'
expect_code "不支持的 charset → 415" 415
head_has "  标记原因" 'x-gateway-reason: unsupported-media-type'

req POST "$GATEWAY/api/body-check" -H 'Content-Type: application/json' \
  -H 'Content-Encoding: gzip' --data-binary '{"sku":"A-1","qty":2}'
expect_code "Content-Encoding 非 identity → 415" 415
head_has "  标记原因" 'x-gateway-reason: unsupported-media-type'

req POST "$GATEWAY/api/body-check" -H 'Content-Type: application/json' --data-binary ''
expect_code "空 body 按空对象校验 → 400" 400
head_has "  缺必填字段" 'x-gateway-field: body.qty'

# 异步读取路径：body 分片到达时 access handler 返回 NGX_DONE，post handler 完成判定后继续 proxy。
printf '{"sku":"A-1","qty":2}' > "$TMP/bigbody.json"
head -c 16000 /dev/zero | tr '\0' ' ' >> "$TMP/bigbody.json"
req POST "$GATEWAY/api/body-check" -H 'Content-Type: application/json' \
  --limit-rate 64k --data-binary "@$TMP/bigbody.json"
expect_code "临时文件/异步读取 body 仍放行 → 200" 200
head_has "  响应来自 demo_server" 'x-demo-backend: 1'

# 超过 client_max_body_size：nginx 可能在插件返回前直接 413（协议允许没有网关头）。
head -c 1100000 /dev/zero | tr '\0' 'a' > "$TMP/toobig.json"
req POST "$GATEWAY/api/body-check" -H 'Content-Type: application/json' \
  --data-binary "@$TMP/toobig.json"
expect_code "body 超过 client_max_body_size → 413" 413

if grep -q 'field=body.qty rule=min' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录 body 范围错误（rule=min）"
else
  bad "日志记录 body 范围错误（rule=min）" "未找到 'field=body.qty rule=min'"
fi

if grep -q 'field=body.is_admin rule=unknown' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录 body 未声明字段（rule=unknown）"
else
  bad "日志记录 body 未声明字段（rule=unknown）" "未找到 'field=body.is_admin rule=unknown'"
fi

if grep -q 'field=body.qty rule=duplicate' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录 body 重复键（rule=duplicate）"
else
  bad "日志记录 body 重复键（rule=duplicate）" "未找到 'field=body.qty rule=duplicate'"
fi

section "5.10 路径参数校验"
# 契约 /info 的 /api/user/{id}：id int, min 1，值是原始未解码路径段。

req GET "$GATEWAY/api/user/42"
expect_code "路径参数合法 → 200" 200

req GET "$GATEWAY/api/user/abc"
expect_code "路径参数 int 语法不符 → 400" 400
head_has "  标记原因" 'x-gateway-reason: invalid-parameter'
head_has "  指向路径字段" 'x-gateway-field: path.id'
head_lacks "  未落到后端" 'x-demo-backend:'

req GET "$GATEWAY/api/user/0"
expect_code "路径参数低于 min → 400" 400
head_has "  指向路径字段" 'x-gateway-field: path.id'

req GET "$GATEWAY/api/user/%34%32"
expect_code "路径参数用原始值（不解码）→ 400" 400
head_has "  指向路径字段" 'x-gateway-field: path.id'

if grep -q 'field=path.id rule=int' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录路径参数类型错误（rule=int）"
else
  bad "日志记录路径参数类型错误（rule=int）" "未找到 'field=path.id rule=int'"
fi

if grep -q 'field=path.id rule=min' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录路径参数范围错误（rule=min）"
else
  bad "日志记录路径参数范围错误（rule=min）" "未找到 'field=path.id rule=min'"
fi

section "5.11 分组路由（headers / unmatched / path_regex）"
# 契约 /info 的分组：
#   prefix /api/group
#   unmatched forward
#   path_regex ^/api/group/(items/[0-9]+|admin/audit)$
#   headers x-group-token: true
#   /items/{id}：id int min 1
#   /admin（unmatched deny, headers x-admin-token: true）→ /audit

req GET "$GATEWAY/api/group/items/42" -H 'x-group-token: 1'
expect_code "分组路径 + 继承 header 合法 → 200" 200
head_has "  响应来自 demo_server" 'x-demo-backend: 1'
body_has "  路径参数透传到后端" '"id":"42"'

req GET "$GATEWAY/api/group/items/42"
expect_code "分组 header 缺失 → 400" 400
head_has "  标记原因" 'x-gateway-reason: missing-parameter'
head_has "  指向继承的 header" 'x-gateway-field: header.x-group-token'
head_lacks "  未落到后端" 'x-demo-backend:'

req GET "$GATEWAY/api/group/items/0" -H 'x-group-token: 1'
expect_code "分组路由的路径参数仍校验 → 400" 400
head_has "  指向路径字段" 'x-gateway-field: path.id'

req GET "$GATEWAY/api/group/admin/audit" -H 'x-group-token: 1'
expect_code "嵌套分组 header 缺失 → 400" 400
head_has "  指向嵌套 header" 'x-gateway-field: header.x-admin-token'

req GET "$GATEWAY/api/group/admin/audit" -H 'x-group-token: 1' -H 'x-admin-token: 1'
expect_code "嵌套分组两个 header 都满足 → 200" 200
head_has "  响应来自 demo_server" 'x-demo-backend: 1'

req GET "$GATEWAY/api/group/admin/unknown" -H 'x-group-token: 1' -H 'x-admin-token: 1'
expect_code "嵌套分组 unmatched=deny → 404" 404
head_has "  标记原因" 'x-gateway-reason: route-not-declared'
head_lacks "  未落到后端" 'x-demo-backend:'

# path_regex 不通过 = 路径未命中：按分组 unmatched=forward 交给后端，
# 不会再执行分组 header / 路径参数校验。
req GET "$GATEWAY/api/group/items/abc"
expect_code "分组 path_regex 失败 → forward 到后端" 200
head_has "  响应来自 demo_server" 'x-demo-backend: 1'
head_lacks "  不是网关拒绝" 'x-gateway-reason'

req GET "$GATEWAY/api/group/other/42"
expect_code "分组未命中叶子路由 → forward" 404
head_has "  响应来自后端而不是网关" 'x-demo-backend: 1'
head_lacks "  不是网关拒绝" 'x-gateway-reason'

if grep -q 'field=header.x-group-token rule=required' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录分组 header 缺失（rule=required）"
else
  bad "日志记录分组 header 缺失（rule=required）" "未找到 'field=header.x-group-token rule=required'"
fi

if grep -q 'unmatched=forward, scope=/api/group' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录分组 forward 的 scope"
else
  bad "日志记录分组 forward 的 scope" "未找到 'unmatched=forward, scope=/api/group'"
fi

section "5.12 分组业务示例（登录 header / 表单 / 嵌套 admin）"
# 契约 /info 的示例分组：
#   /api/v1 组级 authorization（char 8..128）
#     GET /me、GET /orders/{id}（id int min 1）、POST /feedback（JSON/form）
#     /admin（x-admin-token）→ GET /audit
#   /api/public/survey（公开表单）

req GET "$GATEWAY/api/v1/me"
expect_code "登录分组缺 authorization → 400" 400
head_has "  标记原因" 'x-gateway-reason: missing-parameter'
head_has "  指向分组 header" 'x-gateway-field: header.authorization'
head_lacks "  未落到后端" 'x-demo-backend:'

req GET "$GATEWAY/api/v1/me" -H 'Authorization: x'
expect_code "authorization 太短 → 400" 400
head_has "  指向分组 header" 'x-gateway-field: header.authorization'

req GET "$GATEWAY/api/v1/me" -H 'Authorization: Bearer demo-token-0001'
expect_code "登录 header 合法 → 200" 200
head_has "  响应来自 demo_server" 'x-demo-backend: 1'
body_has "  后端识别登录用户" '"user":"demo"'

req GET "$GATEWAY/api/v1/orders/42" -H 'Authorization: Bearer demo-token-0001'
expect_code "分组 + 路径参数合法 → 200" 200
body_has "  订单号透传" '"order_id":"42"'

req GET "$GATEWAY/api/v1/orders/0" -H 'Authorization: Bearer demo-token-0001'
expect_code "分组路径参数 min 校验 → 400" 400
head_has "  指向路径字段" 'x-gateway-field: path.id'

# 分组 path_regex 不通过：先按未命中处理，不再检查 header / 路径参数。
req GET "$GATEWAY/api/v1/orders/abc" -H 'Authorization: Bearer demo-token-0001'
expect_code "分组 path_regex 失败 → 404" 404
head_has "  标记原因" 'x-gateway-reason: route-not-declared'
head_lacks "  未落到后端" 'x-demo-backend:'

# 表单：feedback 同时支持 form 与 JSON；topic=bug 时 comment 条件必填。
req POST "$GATEWAY/api/v1/feedback" -H 'Authorization: Bearer demo-token-0001' \
  -H 'Content-Type: application/x-www-form-urlencoded' --data-binary 'rating=5&topic=feature'
expect_code "form feedback 合法 → 200" 200
head_has "  响应来自 demo_server" 'x-demo-backend: 1'
body_has "  form 字段解析" '"rating":5'

req POST "$GATEWAY/api/v1/feedback" -H 'Authorization: Bearer demo-token-0001' \
  -H 'Content-Type: application/x-www-form-urlencoded' --data-binary 'rating=5&topic=bug'
expect_code "form 条件必填缺失 → 400" 400
head_has "  标记原因" 'x-gateway-reason: missing-parameter'
head_has "  指向 body 字段" 'x-gateway-field: body.comment'
head_lacks "  未落到后端" 'x-demo-backend:'

req POST "$GATEWAY/api/v1/feedback" -H 'Authorization: Bearer demo-token-0001' \
  -H 'Content-Type: application/x-www-form-urlencoded' --data-binary 'rating=5&topic=bug&comment=broken'
expect_code "form 条件字段存在 → 200" 200
body_has "  注释透传" '"comment":"broken"'

req POST "$GATEWAY/api/v1/feedback" -H 'Authorization: Bearer demo-token-0001' \
  -H 'Content-Type: application/json' --data-binary '{"rating":4,"topic":"feature"}'
expect_code "JSON feedback 合法 → 200" 200
body_has "  JSON 字段解析" '"rating":4'

req POST "$GATEWAY/api/v1/feedback" -H 'Authorization: Bearer demo-token-0001' \
  -H 'Content-Type: application/json' --data-binary '{"rating":4,"topic":"feature","is_admin":true}'
expect_code "form/JSON 未声明字段拒绝 → 400" 400
head_has "  指向未声明字段" 'x-gateway-field: body.is_admin'

# 嵌套分组：/api/v1/admin 追加 x-admin-token。
req GET "$GATEWAY/api/v1/admin/audit" -H 'Authorization: Bearer demo-token-0001'
expect_code "嵌套 admin 缺 x-admin-token → 400" 400
head_has "  指向嵌套 header" 'x-gateway-field: header.x-admin-token'

req GET "$GATEWAY/api/v1/admin/audit" -H 'Authorization: Bearer demo-token-0001' -H 'x-admin-token: 1'
expect_code "嵌套 admin 两个 header 都满足 → 200" 200
body_has "  admin 接口返回" '"audit":true'

# 公开表单分组：不需要登录 header；email 全匹配正则、score 范围、consent 布尔。
req POST "$GATEWAY/api/public/survey" \
  -H 'Content-Type: application/x-www-form-urlencoded' --data-binary 'email=a%40b.com&score=5&consent=true'
expect_code "公开 form survey 合法 → 200" 200
head_has "  响应来自 demo_server" 'x-demo-backend: 1'
body_has "  email 解码并透传" '"email":"a@b.com"'

req POST "$GATEWAY/api/public/survey" \
  -H 'Content-Type: application/x-www-form-urlencoded' --data-binary 'score=5&consent=true'
expect_code "survey 缺 email → 400" 400
head_has "  指向 body 字段" 'x-gateway-field: body.email'

req POST "$GATEWAY/api/public/survey" \
  -H 'Content-Type: application/x-www-form-urlencoded' --data-binary 'email=nope&score=5&consent=true'
expect_code "survey email 正则失败 → 400" 400
head_has "  指向 body 字段" 'x-gateway-field: body.email'

req POST "$GATEWAY/api/public/survey" \
  -H 'Content-Type: application/json' --data-binary '{"email":"c@d.com","score":4,"consent":false}'
expect_code "公开 JSON survey 合法 → 200" 200
body_has "  JSON 字段解析" '"score":4'

req GET "$GATEWAY/api/v1/unknown" -H 'Authorization: Bearer demo-token-0001'
expect_code "登录分组未声明路径 → 404" 404
head_has "  标记原因" 'x-gateway-reason: route-not-declared'
head_lacks "  未落到后端" 'x-demo-backend:'

if grep -q 'field=header.authorization rule=min_len' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录分组 authorization 长度（rule=min_len）"
else
  bad "日志记录分组 authorization 长度（rule=min_len）" "未找到 'field=header.authorization rule=min_len'"
fi

if grep -q 'field=body.comment rule=required_if' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录表单条件必填（rule=required_if）"
else
  bad "日志记录表单条件必填（rule=required_if）" "未找到 'field=body.comment rule=required_if'"
fi

if grep -q 'field=body.email rule=regex' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录表单正则失败（rule=regex）"
else
  bad "日志记录表单正则失败（rule=regex）" "未找到 'field=body.email rule=regex'"
fi

section "5.13 分组剩余继承（methods / query / body / path_params）"
# /api/v1 组级：methods GET/POST、query trace、body locale；
# /api/org 组级 path_params org int。

req GET "$GATEWAY/api/v1/me?trace=abc" -H 'Authorization: Bearer demo-token-0001'
expect_code "分组 query 继承合法 → 200" 200

req GET "$GATEWAY/api/v1/me?trace=123456789" -H 'Authorization: Bearer demo-token-0001'
expect_code "分组 query 超长 → 400" 400
head_has "  指向继承的 query" 'x-gateway-field: query.trace'
head_lacks "  未落到后端" 'x-demo-backend:'

req PUT "$GATEWAY/api/v1/me" -H 'Authorization: Bearer demo-token-0001'
expect_code "分组 methods 与路由取交集 → 405" 405
head_has "  Allow 只含有效方法" 'allow: GET'

req POST "$GATEWAY/api/v1/feedback" -H 'Authorization: Bearer demo-token-0001' \
  -H 'Content-Type: application/x-www-form-urlencoded' --data-binary 'rating=5&topic=feature&locale=zh'
expect_code "分组 body 与路由 body 合并 → 200" 200
head_has "  响应来自 demo_server" 'x-demo-backend: 1'

req POST "$GATEWAY/api/v1/feedback" -H 'Authorization: Bearer demo-token-0001' \
  -H 'Content-Type: application/x-www-form-urlencoded' --data-binary 'rating=5&topic=feature&locale=zh-cn'
expect_code "分组 body 字段超长 → 400" 400
head_has "  指向继承的 body" 'x-gateway-field: body.locale'

req GET "$GATEWAY/api/org/42/profile"
expect_code "分组 path_params 合法 → 200" 200
body_has "  org 透传" '"org":"42"'

req GET "$GATEWAY/api/org/abc/profile"
expect_code "分组 path_params 非法 → 400" 400
head_has "  指向继承的路径字段" 'x-gateway-field: path.org'
head_lacks "  未落到后端" 'x-demo-backend:'

if grep -q 'field=query.trace rule=max_len' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录分组 query 继承（rule=max_len）"
else
  bad "日志记录分组 query 继承（rule=max_len）" "未找到 'field=query.trace rule=max_len'"
fi

if grep -q 'field=body.locale rule=max_len' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录分组 body 继承（rule=max_len）"
else
  bad "日志记录分组 body 继承（rule=max_len）" "未找到 'field=body.locale rule=max_len'"
fi

if grep -q 'field=path.org rule=int' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录分组 path_params 继承（rule=int）"
else
  bad "日志记录分组 path_params 继承（rule=int）" "未找到 'field=path.org rule=int'"
fi

section "5.14 filters（SQLi / XSS）"
# 契约 /info 的 /api/filter 分组：filters sqli+xss；
# query kw、header x-filter-note、body comment；route 的 sqli:false 不能关闭组级开关。

req GET "$GATEWAY/api/filter/search?kw=union+select+1"
expect_code "query SQLi 命中 → 400" 400
head_has "  标记原因" 'x-gateway-reason: invalid-parameter'
head_has "  指向字段" 'x-gateway-field: query.kw'
head_lacks "  未落到后端" 'x-demo-backend:'

req GET "$GATEWAY/api/filter/search?kw=%3Cscript%3Ealert(1)%3C%2Fscript%3E"
expect_code "query XSS 命中 → 400" 400
head_has "  指向字段" 'x-gateway-field: query.kw'

req GET "$GATEWAY/api/filter/search?kw=hello"
expect_code "正常 query 不误报 → 200" 200

req GET "$GATEWAY/api/filter/search?extra=%3Cscript%3Ealert(1)%3C%2Fscript%3E"
expect_code "未声明 query 也被过滤 → 400" 400
head_has "  指向未声明字段" 'x-gateway-field: query.extra'

req GET "$GATEWAY/api/filter/header" -H 'x-filter-note: <script>alert(1)</script>'
expect_code "声明 header 的 XSS → 400" 400
head_has "  指向 header" 'x-gateway-field: header.x-filter-note'
head_lacks "  未落到后端" 'x-demo-backend:'

req GET "$GATEWAY/api/filter/header" -H 'x-other: <script>alert(1)</script>'
expect_code "未声明 header 不扫描 → 200" 200
head_has "  响应来自 demo_server" 'x-demo-backend: 1'

req POST "$GATEWAY/api/filter/comment" -H 'Content-Type: application/json' \
  --data-binary '{"comment":"<script>alert(1)</script>","rating":5}'
expect_code "JSON body XSS → 400" 400
head_has "  指向 body" 'x-gateway-field: body.comment'
head_lacks "  未落到后端" 'x-demo-backend:'

req POST "$GATEWAY/api/filter/comment" -H 'Content-Type: application/x-www-form-urlencoded' \
  --data-binary 'comment=%3Cscript%3Ealert(1)%3C%2Fscript%3E&rating=5'
expect_code "form body XSS → 400" 400
head_has "  指向 body" 'x-gateway-field: body.comment'

req POST "$GATEWAY/api/filter/comment" -H 'Content-Type: application/json' \
  --data-binary '{"comment":"hello","rating":5}'
expect_code "正常 body 不误报 → 200" 200

req POST "$GATEWAY/api/filter/comment" -H 'Content-Type: application/json' \
  --data-binary '{"comment":"<script>alert(1)</script>","rating":"abc"}'
expect_code "body 参数错误优先于过滤 → 400" 400
head_has "  报 int 而不是 filter" 'x-gateway-field: body.rating'
head_lacks "  不是过滤错误" 'x-gateway-field: body.comment'

# 路由写 sqli:false 不能关闭组级 sqli。
req POST "$GATEWAY/api/filter/comment?kw=union+select+1" \
  -H 'Content-Type: application/json' --data-binary '{"comment":"hello","rating":5}'
expect_code "route false 不关闭组级 sqli → 400" 400
head_has "  指向 query" 'x-gateway-field: query.kw'

if grep -q 'field=query.kw rule=filter filter=sqli' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录 SQLi 过滤（filter=sqli）"
else
  bad "日志记录 SQLi 过滤（filter=sqli）" "未找到 'field=query.kw rule=filter filter=sqli'"
fi

if grep -q 'field=body.comment rule=filter filter=xss' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录 XSS 过滤（filter=xss）"
else
  bad "日志记录 XSS 过滤（filter=xss）" "未找到 'field=body.comment rule=filter filter=xss'"
fi

if grep -q 'field=header.x-filter-note rule=filter filter=xss' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录 header 过滤（filter=xss）"
else
  bad "日志记录 header 过滤（filter=xss）" "未找到 'field=header.x-filter-note rule=filter filter=xss'"
fi

# filters scope：/api/filter/scope 只在 query 上开启过滤，其它维度放行。
req GET "$GATEWAY/api/filter/scope/query?kw=union+select+1"
expect_code "scope=query：query SQLi → 400" 400
head_has "  指向 query" 'x-gateway-field: query.kw'
head_lacks "  未落到后端" 'x-demo-backend:'

req GET "$GATEWAY/api/filter/scope/query?kw=%3Cscript%3Ealert(1)%3C%2Fscript%3E"
expect_code "scope=query：query XSS → 400" 400
head_has "  指向 query" 'x-gateway-field: query.kw'

req POST "$GATEWAY/api/filter/scope/body" -H 'Content-Type: application/json' \
  --data-binary '{"comment":"union select 1"}'
expect_code "scope=query：body 不扫描，SQLi 载荷放行 → 200" 200
head_has "  请求到达后端" 'x-demo-backend: 1'

req GET "$GATEWAY/api/filter/scope/header" -H 'x-filter-note: <script>alert(1)</script>'
expect_code "scope=query：header 不扫描，XSS 载荷放行 → 200" 200
head_has "  请求到达后端" 'x-demo-backend: 1'

req GET "$GATEWAY/api/filter/scope/item/union%20select%201"
expect_code "scope=query：path 不扫描，SQLi 载荷放行 → 200" 200
head_has "  请求到达后端" 'x-demo-backend: 1'

section "5.15 响应覆盖（responses）"
# demo 的 /info 顶层 responses 覆盖了 404 / 405 / 400 / 415 的 body；
# 其中 unsupported_media_type 用 415 演示状态码覆盖。

req GET "$GATEWAY/api/not-declared"
expect_code "自定义 404 状态" 404
head_has "  X-Gateway-Reason 保留" 'x-gateway-reason: route-not-declared'
head_has "  Content-Type" 'content-type: application/json'
body_has "  自定义 404 body" '"error":"route-denied"'

req POST "$GATEWAY/api/health"
expect_code "自定义 405 状态" 405
head_has "  Allow 仍然保留" 'allow: GET'
head_has "  X-Gateway-Reason 保留" 'x-gateway-reason: method-not-allowed'
body_has "  自定义 405 body" '"error":"method-denied"'

req GET "$GATEWAY/api/header-check"
expect_code "自定义 400 missing 状态" 400
head_has "  X-Gateway-Field 保留" 'x-gateway-field: header.x-demo-token'
body_has "  自定义 missing body" '"error":"missing-parameter"'

req POST "$GATEWAY/api/body-check" -H 'Content-Type: application/json' \
  --data-binary '{"sku":"A-1","qty":0}'
expect_code "自定义 400 invalid 状态" 400
head_has "  X-Gateway-Field 保留" 'x-gateway-field: body.qty'
body_has "  自定义 invalid body" '"error":"invalid-parameter"'

req POST "$GATEWAY/api/body-check" -H 'Content-Type: text/plain' --data-binary 'x'
expect_code "自定义 415 状态" 415
head_has "  X-Gateway-Reason 保留" 'x-gateway-reason: unsupported-media-type'
body_has "  自定义 415 body" '"error":"unsupported-media-type"'

section "5.16 路由级 path_regex 与 priority"
# /api/route-regex/{id}：path_regex 只允许 4 位数字。
# /api/priority/{id} priority=10，/api/priority/me priority=0；高优先级参数路由先匹配。

req GET "$GATEWAY/api/route-regex/2026"
expect_code "路由级 path_regex 匹配 → 200" 200
head_has "  响应来自 demo_server" 'x-demo-backend: 1'
body_has "  路径参数透传" '"id":"2026"'

req GET "$GATEWAY/api/route-regex/26"
expect_code "路由级 path_regex 失败 → 404" 404
head_has "  标记原因" 'x-gateway-reason: route-not-declared'
head_lacks "  未落到后端" 'x-demo-backend:'

req GET "$GATEWAY/api/priority/me"
expect_code "高优先级参数路由覆盖静态路由 → 400" 400
head_has "  指向参数规则" 'x-gateway-field: path.id'
head_lacks "  未落到后端" 'x-demo-backend:'

req GET "$GATEWAY/api/priority/x"
expect_code "高优先级参数路由正常匹配 → 200" 200
body_has "  参数路由 handler" '"matched":"param"'

req GET "$GATEWAY/api/priority/42"
expect_code "priority 参数路由的路径规则仍生效 → 400" 400
head_has "  指向参数规则" 'x-gateway-field: path.id'

section "5.17 Body 大小上限（body.max_bytes）"
# /api/body-check：max_bytes=65536；Content-Length 超限在读 body 前拒绝。
python3 - "$TMP" <<'PY'
import json, os, sys
root = sys.argv[1]
with open(os.path.join(root, "body-100k.json"), "w") as f:
    f.write(json.dumps({"sku": "A1", "qty": 2, "note": "x" * 100000}, separators=(",", ":")))
with open(os.path.join(root, "limit-1k.json"), "w") as f:
    f.write(json.dumps({"data": "x" * 1000}, separators=(",", ":")))
with open(os.path.join(root, "limit-3k.json"), "w") as f:
    f.write(json.dumps({"data": "x" * 3000}, separators=(",", ":")))
PY

req POST "$GATEWAY/api/body-check" -H 'Content-Type: application/json' \
  --data-binary "@$TMP/body-100k.json"
expect_code "Content-Length 超过 max_bytes → 413" 413
head_has "  reason=body-too-large" 'x-gateway-reason: body-too-large'
head_has "  标记网关" 'x-gateway-name: myrustwebdemo1'
body_has "  responses.body_too_large 自定义 body" '"error":"body-too-large"'
body_lacks "  请求未到后端" 'x-demo-backend:'
if grep -q 'reason=body-too-large limit=65536 gateway=myrustwebdemo1' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录 body-too-large"
else
  bad "日志记录 body-too-large" "未找到 'reason=body-too-large limit=65536 gateway=myrustwebdemo1'"
fi

# chunked 没有 Content-Length：读完后再判，仍然 413。
req POST "$GATEWAY/api/body-check" -H 'Content-Type: application/json' \
  -H 'Transfer-Encoding: chunked' --data-binary "@$TMP/body-100k.json"
expect_code "chunked 超过 max_bytes → 413" 413
head_has "  reason=body-too-large" 'x-gateway-reason: body-too-large'

# JSON 结构扫描：在读入/解析前限制深度、字符串长度、数组元素数。
req POST "$GATEWAY/api/body-check" -H 'Content-Type: application/json' \
  --data-binary '{"sku":"A-1","qty":2,"x":{"a":{"b":{"c":{"d":1}}}}}'
expect_code "JSON 嵌套深度超过 max_depth → 400" 400
head_has "  reason=invalid-parameter" 'x-gateway-reason: invalid-parameter'
body_has "  responses.invalid_parameter 自定义 body" '"error":"invalid-parameter"'
body_lacks "  请求未到后端" 'x-demo-backend:'
if grep -q 'rule=json-depth' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录 json-depth"
else
  bad "日志记录 json-depth" "未找到 'rule=json-depth'"
fi

req POST "$GATEWAY/api/body-check" -H 'Content-Type: application/json' \
  --data-binary '{"sku":"A-1","qty":2,"note":"123456789012345678901234567890123"}'
expect_code "JSON 字符串超过 max_string_length → 400" 400
if grep -q 'rule=json-string-length' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录 json-string-length"
else
  bad "日志记录 json-string-length" "未找到 'rule=json-string-length'"
fi

req POST "$GATEWAY/api/body-check" -H 'Content-Type: application/json' \
  --data-binary '{"sku":"A-1","qty":2,"x":[1,2,3,4,5,6]}'
expect_code "JSON 数组元素超过 max_array_entries → 400" 400
if grep -q 'rule=json-array-entries' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录 json-array-entries"
else
  bad "日志记录 json-array-entries" "未找到 'rule=json-array-entries'"
fi

# 分组继承：组级 2K；子级可收紧到 512 或写 0 关闭。
req POST "$GATEWAY/api/limit/inherit" -H 'Content-Type: application/json' \
  --data-binary "@$TMP/limit-3k.json"
expect_code "组级 max_bytes 继承：3K → 413" 413
head_has "  reason=body-too-large" 'x-gateway-reason: body-too-large'
body_has "  自定义 body" '"error":"body-too-large"'

req POST "$GATEWAY/api/limit/inherit" -H 'Content-Type: application/json' \
  --data-binary "@$TMP/limit-1k.json"
expect_code "组级 max_bytes 以内 → 200" 200
head_has "  请求到达后端" 'x-demo-backend: 1'

req POST "$GATEWAY/api/limit/small" -H 'Content-Type: application/json' \
  --data-binary "@$TMP/limit-1k.json"
expect_code "子级收紧到 512：1K → 413" 413

req POST "$GATEWAY/api/limit/disabled" -H 'Content-Type: application/json' \
  --data-binary "@$TMP/limit-3k.json"
expect_code "子级 max_bytes=0 关闭上限：3K → 200" 200
body_has "  后端收到完整 body" '"bytes":3011'

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

if grep -q '\[myrustwebdemo2\] contract loaded from url protocol=1 version=phase1-forward routes=8 unmatched=forward' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "日志记录了 forward 契约的未声明策略"
else
  bad "日志记录了 forward 契约的未声明策略" "未找到 unmatched=forward"
fi

section "8. 日志"
if grep -q '\[myrustwebdemo1\] contract loaded from url protocol=1 version=phase1-fixed routes=36 unmatched=deny' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "error_log 记录了契约加载"
else
  bad "error_log 记录了契约加载" "未找到 '[myrustwebdemo1] contract loaded from url protocol=1 version=phase1-fixed routes=36 unmatched=deny'"
fi

if grep -q '\[myrustwebdemo1\] contract loaded' "$ROOT/nginx/logs/error.log" 2>/dev/null \
  && grep -q '\[myrustwebdemo2\] contract loaded' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "两个网关各自拉取契约"
else
  bad "两个网关各自拉取契约" "error_log 缺少 myrustwebdemo1 / myrustwebdemo2 的 contract loaded"
fi

if grep -q '\[myrustwebdemo2\] contract loaded from url protocol=1 version=phase1-forward routes=8 unmatched=forward' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "demo2 拉到的是 /info_v2 的契约"
else
  bad "demo2 拉到的是 /info_v2 的契约" "未找到 '[myrustwebdemo2] contract loaded from url protocol=1 ... unmatched=forward'"
fi

if grep -q 'contract manager: 2 gateway(s) configured' "$ROOT/nginx/logs/error.log" 2>/dev/null; then
  ok "error_log 记录契约来源数量"
else
  bad "error_log 记录契约来源数量" "未找到 'contract manager: 2 gateway(s) configured'"
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

# TTL 已移除：旧指令现在应被 nginx 当成未知指令拒绝，避免静默失效。
sed 's|gateway_info_url myrustwebdemo2 http://127.0.0.1:9000/info_v2;|&\n    gateway_url_ttl myrustwebdemo2 5;|' \
  "$BASE_CONF" >"$TMP/t-ttl-gone.conf"
if nginx_t "$TMP/t-ttl-gone.conf"; then
  bad "已移除的 gateway_url_ttl 被拒绝" "nginx -t 竟然通过"
else
  ok "已移除的 gateway_url_ttl 被拒绝"
fi
if grep -q 'unknown directive "gateway_url_ttl"' "$TMP/nginx-t.log"; then
  ok "  错误信息说明指令已移除"
else
  bad "  错误信息说明指令已移除" "$(head -3 "$TMP/nginx-t.log")"
fi

# gateway_diagnostics：只接受 on|off，重复声明拒绝，打开时打 WARN（便于审计生产配置）。
sed 's/gateway_diagnostics on;/gateway_diagnostics maybe;/' "$BASE_CONF" >"$TMP/t-diag-bad.conf"
if nginx_t "$TMP/t-diag-bad.conf"; then
  bad "非法 gateway_diagnostics 取值被拒绝" "nginx -t 竟然通过"
else
  ok "非法 gateway_diagnostics 取值被拒绝"
fi
if grep -q 'must be `on` or `off`' "$TMP/nginx-t.log"; then
  ok "  错误信息说明只接受 on|off"
else
  bad "  错误信息说明只接受 on|off" "$(head -3 "$TMP/nginx-t.log")"
fi

sed 's/gateway_diagnostics on;/gateway_diagnostics on;\n    gateway_diagnostics off;/' \
  "$BASE_CONF" >"$TMP/t-diag-dup.conf"
if nginx_t "$TMP/t-diag-dup.conf"; then
  bad "重复 gateway_diagnostics 被拒绝" "nginx -t 竟然通过"
else
  ok "重复 gateway_diagnostics 被拒绝"
fi
if grep -q 'duplicate `gateway_diagnostics`' "$TMP/nginx-t.log"; then
  ok "  错误信息说明重复声明"
else
  bad "  错误信息说明重复声明" "$(head -3 "$TMP/nginx-t.log")"
fi

if nginx_t "$BASE_CONF" && grep -q 'gateway diagnostics enabled' "$TMP/nginx-t.log"; then
  ok "打开诊断头时 nginx -t 打 WARN（便于审计）"
else
  bad "打开诊断头时 nginx -t 打 WARN（便于审计）" "$(head -3 "$TMP/nginx-t.log")"
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
    gateway_diagnostics on;
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

# 只改文件、不 reload：不应有任何变化（没有周期轮询）
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

# reload 后旧 worker 会先把已接受的连接处理完再退出，刚切过去时可能仍命中旧契约；
# 轮询等待旧 worker 退出，避免把正常的新旧交替误判成失败。
for _ in $(seq 1 50); do
  req GET "$FILE_GW/api/health"
  [ "$CODE" = "404" ] && break
  sleep 0.1
done
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

section "12. 契约拉取不跟随重定向（独立实例）"
URL_PREFIX="$TMP/urlsrc"
URL_GW="http://127.0.0.1:18098"
mkdir -p "$URL_PREFIX/conf" "$URL_PREFIX/logs"
url_nginx() { "$ROOT/nginx/sbin/nginx" -p "$URL_PREFIX" -c conf/nginx.conf "$@"; }

cat >"$URL_PREFIX/conf/nginx.conf" <<CONF
worker_processes 1;
error_log logs/error.log notice;
pid logs/nginx.pid;
load_module $ROOT/nginx/modules/libmy_gateway.so;
events { worker_connections 64; }
http {
    access_log off;
    gateway_diagnostics on;
    gateway_name rd;
    gateway_info_url rd http://127.0.0.1:9000/info-redirect;

    # 声明了但没有任何 location 引用：不应被 ContractManager 拉取。
    gateway_name unused;
    gateway_info_url unused http://127.0.0.1:9000/info;
    server {
        listen 127.0.0.1:18098;
        location /rd/ { enable_gateway_by rd; proxy_pass http://127.0.0.1:9000; }
    }
}
CONF

if url_nginx -t >"$TMP/nginx-t-urlsrc.log" 2>&1 && url_nginx >>"$TMP/nginx-t-urlsrc.log" 2>&1; then
  ok "302 契约地址实例启动"
else
  bad "302 契约地址实例启动" "$(head -3 "$TMP/nginx-t-urlsrc.log")"
fi

for _ in $(seq 1 50); do
  grep -q 'contract manager:' "$URL_PREFIX/logs/error.log" 2>/dev/null && break
  sleep 0.1
done
if grep -q 'contract manager: 1 gateway(s) configured' "$URL_PREFIX/logs/error.log" 2>/dev/null; then
  ok "未被 location 引用的网关不加载契约"
else
  bad "未被 location 引用的网关不加载契约" \
    "$(grep -m1 'contract manager:' "$URL_PREFIX/logs/error.log" 2>/dev/null)"
fi
if grep -q '\[unused\] contract loaded' "$URL_PREFIX/logs/error.log" 2>/dev/null; then
  bad "  未引用网关没有加载日志" "出现了 [unused] contract loaded"
else
  ok "  未引用网关没有加载日志"
fi

req GET "$URL_GW/rd/api/health"
expect_code "302 不被跟随：从未加载成功 → 503 fail-closed" 503
head_has "  标记契约未就绪" 'x-gateway-reason: contract-not-ready'

for _ in $(seq 1 50); do
  grep -q '\[rd\] contract refresh failed: unexpected HTTP status 302' "$URL_PREFIX/logs/error.log" 2>/dev/null && break
  req GET "$URL_GW/rd/api/health"
  sleep 0.1
done
grep -q '\[rd\] contract refresh failed: unexpected HTTP status 302' "$URL_PREFIX/logs/error.log" 2>/dev/null \
  && ok "error_log 记录 302 拉取失败（不跟随）" \
  || bad "error_log 记录 302 拉取失败（不跟随）" "$(grep -m1 'contract refresh failed' "$URL_PREFIX/logs/error.log" 2>/dev/null)"

url_nginx -s stop >/dev/null 2>&1

section "13. 契约静态校验：methods / 路径参数名 / 规则合计上限"
CHECK_PREFIX="$TMP/contractcheck"
mkdir -p "$CHECK_PREFIX/conf" "$CHECK_PREFIX/logs"
cat >"$CHECK_PREFIX/conf/nginx.conf" <<EOF
worker_processes 1;
error_log logs/error.log notice;
pid logs/nginx.pid;
load_module $ROOT/nginx/modules/libmy_gateway.so;
events { worker_connections 64; }
http {
    access_log off;
    gateway_name checkgw;
    gateway_info_file checkgw conf/contract.json;
    server {
        listen 127.0.0.1:18097;
        location / { enable_gateway_by checkgw; }
    }
}
EOF

# 只跑 nginx -t（文件模式在配置加载时解析契约，不会真启动实例）。
reject_contract() {
  local name="$1"
  if "$ROOT/nginx/sbin/nginx" -p "$CHECK_PREFIX" -c conf/nginx.conf -t >"$TMP/nginx-t-check.log" 2>&1; then
    bad "$name" "契约被接受了（应拒绝）：$(head -c 120 "$CHECK_PREFIX/conf/contract.json")"
  else
    ok "$name"
  fi
}

cat >"$CHECK_PREFIX/conf/contract.json" <<'JSON'
{"protocol":1,"routes":[{"path":"/x","methods":["GET\r\nX-Injected: pwned"]}]}
JSON
reject_contract "methods 含 CRLF 被拒绝（响应头/日志注入）"
grep -q 'invalid method' "$TMP/nginx-t-check.log" \
  && ok "  错误信息说明 method 非法" \
  || bad "  错误信息说明 method 非法" "$(head -2 "$TMP/nginx-t-check.log")"

cat >"$CHECK_PREFIX/conf/contract.json" <<'JSON'
{"protocol":1,"routes":[{"path":"/x","methods":["GET, POST"]}]}
JSON
reject_contract "methods 含逗号被拒绝（避免拆成两个方法）"

cat >"$CHECK_PREFIX/conf/contract.json" <<'JSON'
{"protocol":1,"routes":[{"path":"/w/{1id}","methods":["GET"]}]}
JSON
reject_contract "非法路径参数名 {1id} 被拒绝"

cat >"$CHECK_PREFIX/conf/contract.json" <<'JSON'
{"protocol":1,"routes":[{"path":"/w/{id}/{id}","methods":["GET"]}]}
JSON
reject_contract "重复路径参数名被拒绝（未写 path_params 也算）"

python3 - "$CHECK_PREFIX/conf/contract.json" <<'PY'
import json, sys

# 128 query + 128 header + 1 body = 257 > 256，但每块都没超过 128。
contract = {
    "protocol": 1,
    "routes": [{
        "path": "/many",
        "methods": ["GET"],
        "query": {"params": {f"q{i}": {"type": "char"} for i in range(128)}},
        "headers": {"params": {f"h{i}": {"type": "char"} for i in range(128)}},
        "body": {"params": {"b0": {"type": "char"}}, "unknown": "ignore"},
    }],
}
with open(sys.argv[1], "w") as handle:
    json.dump(contract, handle)
PY
reject_contract "单路由规则合计 257 条被拒绝"
grep -q 'parameter rules, limit is 256' "$TMP/nginx-t-check.log" \
  && ok "  错误信息说明合计上限" \
  || bad "  错误信息说明合计上限" "$(head -2 "$TMP/nginx-t-check.log")"

section "14. 诊断头默认关闭（上线形态）与 X-Request-Id 关联"
DIAG_PREFIX="$TMP/diagoff"
DIAG_GW="http://127.0.0.1:18096"
mkdir -p "$DIAG_PREFIX/conf" "$DIAG_PREFIX/logs"
diag_nginx() { "$ROOT/nginx/sbin/nginx" -p "$DIAG_PREFIX" -c conf/nginx.conf "$@"; }
DIAG_LOG="$DIAG_PREFIX/logs/error.log"

# 从响应头取一个值（大小写不敏感），用于验证 X-Request-Id 与日志里的 rid 一致。
head_value() {
  tr -d '\r' <"$TMP/head" | grep -i "^$1:" | head -1 | sed 's/^[^:]*:[[:space:]]*//'
}

cat >"$DIAG_PREFIX/conf/contract.json" <<'JSON'
{
  "protocol": 1,
  "version": "diag-off-1",
  "routes": [
    { "path": "/api/health", "methods": ["GET"] },
    { "path": "/api/token", "methods": ["GET"],
      "headers": { "params": { "x-demo-token": { "required": true } } } },
    { "path": "/api/echo", "methods": ["POST"],
      "body": { "params": { "name": { "type": "char", "required": true } }, "max_bytes": 32 } }
  ]
}
JSON

# 故意**不写** gateway_diagnostics：这一节验证的就是默认值 = off（上线形态）。
cat >"$DIAG_PREFIX/conf/nginx.conf" <<EOF
worker_processes 1;
error_log logs/error.log notice;
pid logs/nginx.pid;
load_module $ROOT/nginx/modules/libmy_gateway.so;
events { worker_connections 64; }
http {
    access_log off;
    client_max_body_size 1k;
    gateway_name diagoff;
    gateway_info_file diagoff conf/contract.json;

    # 契约地址永远 302 → 从未加载成功，用来验证 503 也不带诊断头。
    gateway_name unready;
    gateway_info_url unready http://127.0.0.1:9000/info-redirect;

    server {
        listen 127.0.0.1:18096;
        location /unready/ {
            enable_gateway_by unready;
            proxy_pass http://127.0.0.1:9000;
        }
        location / {
            enable_gateway_by diagoff;
            proxy_pass http://127.0.0.1:9000;
        }
    }
}
EOF

if diag_nginx -t >"$TMP/nginx-t-diag.log" 2>&1; then
  ok "默认 off：不写指令也能加载"
else
  bad "默认 off：不写指令也能加载" "$(head -3 "$TMP/nginx-t-diag.log")"
fi
if grep -q 'gateway diagnostics enabled' "$TMP/nginx-t-diag.log"; then
  bad "  默认 off 不打 WARN" "nginx -t 出现了 diagnostics enabled 警告"
else
  ok "  默认 off 不打 WARN"
fi

if diag_nginx >>"$TMP/nginx-t-diag.log" 2>&1; then
  ok "诊断关闭实例启动"
else
  bad "诊断关闭实例启动" "$(head -3 "$TMP/nginx-t-diag.log")"
fi

# 1) 扫描器路径：现在只是未声明路由，诊断头关闭时同样不泄露判定细节。
req GET "$DIAG_GW/phpinfo.php"
expect_code "扫描器路径仍然 404" 404
head_lacks "  不返回 X-Gateway-Name" 'x-gateway-name'
head_has "  仍返回 X-Request-Id" 'x-request-id:'
RID="$(head_value X-Request-Id)"
if [ -n "$RID" ] && grep -q "reason=route-not-declared gateway=diagoff rid=$RID" "$DIAG_LOG"; then
  ok "  日志仍带 reason，且 rid 与响应头一致"
else
  bad "  日志仍带 reason，且 rid 与响应头一致" "rid='$RID'；$(grep -m1 '404' "$DIAG_LOG" 2>/dev/null)"
fi

# 2) 404：这是最容易泄露路由存在性的分支。
req GET "$DIAG_GW/api/nope"
expect_code "未声明路由仍然 404" 404
head_lacks "  不返回 X-Gateway-Name" 'x-gateway-name'
head_lacks "  不返回 X-Gateway-Reason" 'x-gateway-reason'
head_lacks "  不返回 X-Gateway-Field" 'x-gateway-field'
head_lacks "  请求未到后端" 'x-demo-backend:'
RID="$(head_value X-Request-Id)"
if [ -n "$RID" ] && grep -q "reason=route-not-declared gateway=diagoff rid=$RID" "$DIAG_LOG"; then
  ok "  日志仍带 reason，且 rid 与响应头一致"
else
  bad "  日志仍带 reason，且 rid 与响应头一致" "rid='$RID'；$(grep -m1 '404' "$DIAG_LOG" 2>/dev/null)"
fi

# 3) 405：Allow 是协议头，必须继续返回；诊断头不返回。
req POST "$DIAG_GW/api/token"
expect_code "方法不符仍然 405" 405
head_has "  协议头 Allow 仍返回" 'allow: GET'
head_lacks "  不返回 X-Gateway-Reason" 'x-gateway-reason'
head_lacks "  不返回 X-Gateway-Name" 'x-gateway-name'

# 4) 400 缺必填头：字段名只进日志，不进响应。
req GET "$DIAG_GW/api/token"
expect_code "缺必填头仍然 400" 400
head_lacks "  不返回 X-Gateway-Field" 'x-gateway-field'
head_lacks "  不返回 X-Gateway-Reason" 'x-gateway-reason'
head_lacks "  请求未到后端" 'x-demo-backend:'
RID="$(head_value X-Request-Id)"
if [ -n "$RID" ] \
  && grep -q "reason=missing-parameter field=header.x-demo-token rule=required gateway=diagoff rid=$RID" "$DIAG_LOG"; then
  ok "  字段名仍在日志里，rid 一致"
else
  bad "  字段名仍在日志里，rid 一致" "rid='$RID'；$(grep -m1 'missing-parameter' "$DIAG_LOG" 2>/dev/null)"
fi

# 5) 413：reason=body-too-large 也不再回写。
req POST "$DIAG_GW/api/echo" -H 'Content-Type: application/json' \
  --data-binary '{"name":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}'
expect_code "超过 max_bytes 仍然 413" 413
head_lacks "  不返回 X-Gateway-Reason" 'x-gateway-reason'
head_lacks "  不返回 X-Gateway-Name" 'x-gateway-name'
head_has "  仍返回 X-Request-Id" 'x-request-id:'

# 6) 503 冷启动：fail-closed 不变，但也不告诉客户端“契约没就绪”。
req GET "$DIAG_GW/unready/api/health"
expect_code "契约未就绪仍然 503" 503
head_lacks "  不返回 X-Gateway-Reason" 'x-gateway-reason'
head_lacks "  不返回 X-Gateway-Name" 'x-gateway-name'
head_has "  仍返回 X-Request-Id" 'x-request-id:'

# 7) 放行路径不应被误加诊断头。
req GET "$DIAG_GW/api/health"
expect_code "合法请求仍然放行" 200
head_has "  请求到达后端" 'x-demo-backend: 1'
head_lacks "  不返回任何 X-Gateway-*" 'x-gateway-'

diag_nginx -s stop >/dev/null 2>&1

printf '\n\033[1m结果：%d 通过，%d 失败\033[0m\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
