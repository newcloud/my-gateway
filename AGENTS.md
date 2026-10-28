# my_gateway 开发约定（Agent 必读）

本项目按**能力切片**开发。每个切片都必须遵循下面的顺序，参考已完成的 Header 存在性：

> **先写 `gateway-core` 协议测试，再实现 core，最后接入 nginx 并补集成测试。**
> 不允许先改 nginx 适配层、再回头补 core 测试；只有 core 单测、没有 nginx 集成测试的切片不算完成。

## 开发流程

1. **协议先行**：先改 `docs/PROTOCOL.md`，写清字段、状态码、`reason` / `field` / `rule` 与失败优先级。
   未实现的能力继续标 🧪。
2. **先写 core 测试**：在 `crates/gateway-core/tests/protocol_v1.rs` 增加 `s<章节>_*` 用例，覆盖正常、
   缺失/非法、优先级、大小写、重复、契约拒绝等边界；先跑出红色。
3. **实现 core 到全绿**：不引入 nginx 类型，只依赖 `gateway-core` 公开 API。
4. **再接入 nginx**：在 `crates/ngx-gateway/src/lib.rs` 收集请求数据并映射状态码/响应头/日志；
   在 `demo_server/src/main.rs` 增加只使用该切片的契约路由；在 `deploy/nginx.conf` 增加专用 location。
5. **补集成测试**：在 `tests/phase1.sh` 增加真实 nginx 断言，至少覆盖拒绝状态码、`X-Gateway-*`、
   请求未到后端、日志字段，以及未实现能力仍被拒绝。
6. **全量验收**：

   ```bash
   cargo fmt --all -- --check
   cargo clippy --offline -p gateway-core --all-targets
   cargo test --offline -p gateway-core
   ./scripts/build-module.sh && ./scripts/dev.sh restart
   ./tests/phase1.sh
   ```

7. **同步文档**：更新 `docs/PROGRESS.md` 的验证结果与历史、`docs/PROTOCOL_TEST_PLAN.md` 的用例数/集成分工，
   以及 `PLAN.md` / `docs/PHASE1_PLAN.md` 中过期的断言数、路由数与日志样例。

## 硬性约束

- **区分“提议”与“已确认需求”**：可以主动提出功能、方案或风险建议，这是被鼓励的；但必须明确标注
  “这是我的提议 / 待确认”，说明理由、影响和替代方案。用户确认前，不要写进 `docs/PROTOCOL.md` /
  `PLAN.md` 当成既定设计，也不要直接实现成默认行为。
- **临时/演示实现必须登记**：任何“先写死、后续再改”的占位或演示逻辑，都要在 `docs/ISSUES.md`
  登记原因、关联代码和预期删除条件。用户未采纳或需求变化时，同一次修改里删除。
- **需求变化时同步清理**：旧实现因新设计失效时，必须在同一次修改里删除；不能只加新代码、
  把旧逻辑留在仓库里。
- **主动报告删除项**：每次答复除新增/修改外，必须列出删除或保留的旧逻辑；发现历史遗留
  （尤其是 AI 自己加的）要主动指出并处理，不能沉默保留。
- **只开对应切片**：接入 Header 存在性就只启用存在性，接入 Header 值规则才启用值规则；Query / Body / `filters` 不得顺带打开。
- **未实现 = 拒绝加载**：契约里出现尚未实现的字段时整份拒绝，不能静默忽略。
- **fail-closed**：缺失/非法一律拒绝并给出稳定 reason，不能放行到后端。
- **两层都要测**：`gateway-core` 测纯判定；`tests/phase1.sh` 测真实 nginx 的响应头、日志和“未到后端”。
- **纯适配层切片可以没有 core 用例**（例如诊断响应头开关）：必须在 `docs/PROTOCOL_TEST_PLAN.md` §7
  登记原因，并在 `tests/phase1.sh` 用「开 / 关」两组实例覆盖，不能只测打开的那一组。
- 新增/删除 demo 契约路由时，同步更新 `tests/phase1.sh` 里的 `routes=N` 与日志断言。
- 改完代码优先用 `./scripts/dev.sh restart` 验证；不要手动覆盖正在运行的 `.so`。

详细计划见 `PLAN.md` §7.1，测试写法见 `docs/PROTOCOL_TEST_PLAN.md`。
