# `/_gateway_refresh` 设计稿（v2，待实现）

| 项目 | 内容 |
| --- | --- |
| 作者 | **DeepSeek V4.1 Flash 1017**（`deepseek` / `deepseek-flash`） |
| 时间 | **2026-10-27 23:10 CST (+0800)** |
| 状态 | 需求已确认；**代码未实现**，本文为实现前设计 |
| 关联 | `docs/AI_PROPOSALS.md` P-04、`docs/ISSUES.md` #4 |

## 1. 目标

提供 `POST /_gateway_refresh`，由任意 worker 接收请求，**worker 不请求 `/info`**；
leader worker 按 location 里配置的网关名单重新拉取、校验，成功后更新共享内存，
其它 worker 只按 generation 读取新快照。

`gateway_refresh` 的名单在配置里写死：

```nginx
location = /_gateway_refresh {
    gateway_refresh name1,name2;

    # 安全由部署方负责；模块只提供文档和 demo
    allow 127.0.0.1;
    deny all;
}
```

- 只刷新名单里的网关；
- 接口本身不接收“刷新哪个 URL/网关”的外部参数，避免 SSRF；
- 模块**不内置 token/鉴权**，安全依赖 nginx ACL / mTLS / 上游管理面；文档和 demo 写清楚。

## 2. API

| 项 | 约定 |
| --- | --- |
| 方法 | 只接受 `POST`；其他方法 `405` + `Allow: POST` |
| 路径 | 由 location 决定，推荐 `/_gateway_refresh` |
| Query | `?mode=auto\|check\|force`，默认 `auto`；其他值 `400` |
| Body | 忽略 |
| 认证 | 不在模块内实现，交给 nginx 配置 |

### 2.1 返回

刷新结果不映射成 HTTP 状态码：**请求被正常处理就返回 `200`，具体成功/失败看 JSON**。

```json
{
  "status": "ok",
  "results": [
    { "name": "name1", "status": "ok" },
    { "name": "name2", "status": "ok" }
  ]
}
```

```json
{
  "status": "partial",
  "results": [
    { "name": "name1", "status": "ok" },
    { "name": "name2", "status": "failed", "reason": "parse" }
  ]
}
```

```json
{
  "status": "failed",
  "results": [
    { "name": "name1", "status": "failed", "reason": "http" },
    { "name": "name2", "status": "failed", "reason": "parse" }
  ]
}
```

- `status` 只表示本次接口结果：`ok` / `partial` / `failed`；
- 单个网关只看 `results[].status`；
- 不返回 version / route 数；
- 失败原因只给稳定短码：`http` / `timeout` / `parse` / `io`，完整错误写 `error_log`。

只有协议级错误才用非 200：

| 场景 | 状态码 |
| --- | --- |
| 非 POST | `405` + `Allow: POST` |
| `mode` 非法 | `400` |
| 共享内存不可用 | `503` |

## 3. `mode` 模式

| `mode` | 拉取 + 解析 | 成功 | 失败 | 现有流量 |
| --- | --- | --- | --- | --- |
| `auto`（默认） | 是 | 替换该网关快照 | 保留旧快照（last-known-good） | 不受影响 |
| `check` | 是 | **不发布**，只返回校验结果 | **不发布**，只返回错误 | 完全不变 |
| `force` | 是 | 替换该网关快照 | **清空该网关旧快照**，进入加载失败状态 | 该网关请求失败（fail-closed） |

- `check` 就是只校验：只判断能否读取、能否解析，绝不上线；
- `auto` 是安全默认：坏契约不会影响线上；
- `force` 用于演练坏契约/强制下线，必须显式传参。

## 4. 多 worker 流程

```text
任意 worker 收到 POST /_gateway_refresh?mode=auto
        │
        ├─ 不请求 /info
        ├─ 写共享内存：seq + 网关位图 + mode
        └─ 挂起当前请求，定时器等待结果
                ▼
leader worker 的后台循环
        ├─ 看到 pending
        ├─ 按位图逐个拉 /info、parse
        ├─ 按 mode 决定是否发布
        ├─ 写每个网关的结果 + done_seq
        ▼
请求 worker 的定时器看到 done_seq
        └─ 组装 JSON，返回 200
```

- worker 不阻塞睡眠，用 `NGX_DONE` + 定时器轮询结果；
- 其它 worker 在 `generation` 变化后重新 `Gateway::parse`，和 reload 后第一次读取一致；
- 只有 leader 发网络请求，符合“单点获取”。

## 5. 共享内存布局

需要比现有 48 字节头部多一块控制区。建议：

```text
header（64 字节）：
  0  magic u32
  4  payload_len u32
  8  generation u64
 16  heartbeat u64
 24  leader u64
 32  config_epoch u64
 40  refresh_request u64
 48  refresh_done u64
 56  reserved u64

control（header 之后、payload 之前）：
  refresh_mask   u64       // 最多 64 个网关
  refresh_mode   u8        // auto / check / force
  refresh_busy   u8        // 预留
  results[64]    u8 * 2    // status + reason
```

- 网关顺序在所有 worker 中一致（同一份配置），位图可直接映射；
- 第一版限制最多 64 个网关；超过则 `nginx -t` 拒绝；
- 失败详情只存短码，完整错误写日志；
- payload 起始偏移相应后移，需要同步更新 `contract_shm.rs` 设计与文档。

> 当前设计**不考虑并发控制**：多个刷新请求同时到达时不做 409、不排队，
> 以最后一次请求为准，结果区可能被覆盖。这是明确接受的限制，后续需要再补。

> 当前设计**不设专用超时**：等待依赖客户端/nginx 超时；leader 卡住时请求可能长时间挂起。
> 也是明确接受的限制。

## 6. 失败与发布语义

`auto` / `force` 都按网关逐个处理，不能整份覆盖：

```text
成功：raw_by_name[name] = 新契约
失败：
  auto   -> 保持旧值
  force  -> 从快照删除该 name，worker 请求返回 503
check    -> 不修改 raw_by_name，不 publish
```

成功后统一 `publish`；内容没变时 `publish` 会跳过。失败不清空其它网关的快照。

## 7. 配置校验

- `gateway_refresh <name1>,<name2>,...;` 一行写完，逗号分隔；只允许声明一次；
- 每个名字必须指向已声明且被 location 引用的网关；
- 未知网关、没有契约来源、空名字、重复名字、超过 64 个 → `nginx -t` 拒绝；
- 逗号两侧不写空格；如果确实要写空格，用引号包起来，例如 `gateway_refresh "name1, name2";`；
- 默认关闭：没有 `gateway_refresh` 的 location 不提供该接口；
- 与 `enable_gateway_by` 同 location 时刷新逻辑优先，文档建议不要混用。

## 8. Demo

在 `deploy/nginx.conf` 增加：

```nginx
location = /_gateway_refresh {
    gateway_refresh myrustwebdemo1,myrustwebdemo2;
    allow 127.0.0.1;
    deny all;
}
```

```bash
curl -X POST 'http://127.0.0.1:8080/_gateway_refresh?mode=auto'
curl -X POST 'http://127.0.0.1:8080/_gateway_refresh?mode=check'
```

安全说明必须写清：这是管理接口，生产必须限制来源；模块不负责鉴权。

## 9. 测试计划

新增 `phase1.sh` 独立实例（文件契约，方便修改内容）：

1. `?mode=check` + 修改文件 → 返回 JSON `status=ok`，但旧路由仍生效（未发布）；
2. `?mode=auto` + 修改文件 → 返回 `status=ok`，新路由生效、旧路由 404；
3. `?mode=auto` + 坏 JSON → 返回 `status=failed reason=parse`，旧路由仍生效；
4. `?mode=force` + 坏 JSON → 返回 `failed`，该网关请求开始 503（fail-closed）；
5. 非法 `mode=xxx` → `400`；
6. `GET` → `405 + Allow: POST`；
7. 未开启 `gateway_refresh` 的 location → 按普通未声明路由 404；
8. `error_log` 有每个网关的 loaded/failed 记录。

这是一个纯适配层切片，`gateway-core` 无新增判定逻辑；按 `AGENTS.md` 在
`docs/PROTOCOL_TEST_PLAN.md` §7 登记“无 core 用例”的原因。

## 10. 风险与已知限制

- **无并发控制**：同时刷新可能结果互相覆盖；
- **无专用超时**：leader/网络卡住时请求可能长时间挂起；
- **无内置鉴权**：一旦误配到公网 location，任何人都能触发刷新；
- **最多 64 个网关**：位图限制；
- **同步等待占用请求槽**：需要部署方配 `limit_req` / 客户端超时；
- **大契约**：刷新会重新读取和解析，受 `ISSUES.md` #1 的体积问题影响。
