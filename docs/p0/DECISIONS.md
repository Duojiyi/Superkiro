# P0 探针决议汇总 (DECISIONS.md)

> **基线版本**：Kiro IDE 1.0.437 / Agent 1.0.794 (Windows x64)  
> **生成日期**：2026-09-10  
> **适用范围**：Spec §16 全部 11 条技术未知数终结性锁定，解除阻塞进入 Phase P1。

---

## 1. Spec §16 全部 11 条未知数逐条结论与证据索引

| # | 未知数名称 | 决议结论 | 权威证据与实现路径 |
|---|---|---|---|
| **1** | env 改道覆盖面 | **纯 env 无法覆盖对话主面**：`acp-q-client` (`GenerateAssistantResponse`) 构造时固定传入 `endpoint: K2u(d)` (`https://runtime.us-east-1.kiro.dev`)，设置 `isCustomEndpoint: true` 覆盖了环境变量。<br>**决议**：采用轻量扩展桩点方案，在 `extension.js` 注入端点重定向。扩展不在 `product.json` 的 `checksums` 校验范围内，100% 免除完整性弹窗。 | `docs/p0/coverage.md`<br>`extension.js#5469409`<br>`product.json#checksums` |
| **2** | 接管持久性方案 | **决议：VSCode `settings.json` + `extension.js` 精准桩点打标**。<br>放弃脆弱的快捷方式劫持（无法拦截任务栏、开始菜单、右键打开）。安装时备份原始 `extension.js.bak`，写入易回滚的唯一标记补丁。 | `docs/p0/persistence.md` |
| **3** | 1.0.x 登录态存储与回调 | **决议：保持 `~/.aws/sso/cache/kiro-auth-token.json`**。<br>包含 `accessToken`, `refreshToken`, `profileArn`, `expiresAt`, `authMethod: "social"`, `provider: "Google"`。`TokenStorage` 使用 `fs.watchFile` 监听，外部文件改动触发即时热重载，无需重启 IDE。 | `docs/p0/auth.md`<br>`docs/p0/samples/auth_token_sample.json` |
| **4** | event-stream 工具帧真实结构 | **决议：标准 AWS Event Stream 二进制协议**。<br>12 字节 prelude、header、payload (JSON)、4 字节 message_crc32。`toolUseEvent` 包含 `toolUseId`, `name`, `input` (分片 JSON), `stop` (bool)。<br>产出 6 组二进制语料，已通过 Kiro 官方解码器 100% 验证。 | `docs/p0/stream.md`<br>`docs/p0/stream/*.bin`<br>`spikes/p0-stub/test_stream_corpus.js` |
| **5** | Kiro 是否校验 token 签发方 | **决议：完全不校验**。<br>Kiro 客户端只做 `JSON.parse`，并在请求头直接透传 `Authorization: Bearer <token>`。网关可完全自主签发承载 `card_id`, `group_id`, `token_version` 的自签名短 TTL JWT。 | `docs/p0/auth.md`<br>`extension.js#TokenStorage` |
| **6** | 管理面响应 schema | **决议：锁定 4 个管理接口 schema**。<br>`ListAvailableModels`, `getUsageLimits`, `listAvailableSubscriptions`, `ListAvailableProfiles` 真实报文及最小必填字段集已锁定，模型列表与套餐/积分虚拟化数据模型成立。 | `docs/p0/mgmt-schema.md`<br>`docs/p0/samples/mgmt_*.json` |
| **7** | `sessionIdleTimeout` 与保活 | **决议：每 20~25s 发送一次空 `assistantResponseEvent` 保活**。<br>客户端看门狗阈值为预警 60s (`warnMs`)，断流 300s (`timeoutMs`)。空内容帧 `{"content": ""}` 会重置看门狗计时，同时被 UI 层静默过滤，不产生可见空行。 | `docs/p0/stream.md`<br>`extension.js#5454812` (Cuu)<br>`docs/p0/stream/05_empty_keepalive.bin` |
| **8** | `amz-sdk-invocation-id` 稳定性 | **决议：100% 稳定，选为全局幂等键**。<br>AWS SDK 重试机制在循环初始化时生成 UUIDv4，后续所有 retry attempts 复用同一个 invocation-id。天然满足防重发、防双扣结算要求。 | `docs/p0/stream.md`<br>`extension.js#11068898` |
| **9** | 点"停止"客户端行为 | **决议：客户端直接销毁底层 TCP Socket**。<br>点击停止触发 `AbortController`，Node 调用 `ClientRequest.destroy()` 掐断连接，不发送任何取消 RPC。网关检测到 HTTP 客户端断开立即中断上游并结算。 | `docs/p0/stream.md`<br>`extension.js#9271500` |
| **10** | autocomplete 关闭与降级 | **决议：`settings.json` 关闭 + HTTP 429 优雅降级**。<br>配置 `"kiroAgent.enableTabAutocomplete": false`；若用户强行开启，网关对 `GenerateCompletions` 返回 HTTP 429 + `{"__type": "ThrottlingException", "message": "...", "reason": "MONTHLY_REQUEST_COUNT"}`，触发官方额度提示且不崩溃。 | `docs/p0/aux-surfaces.md`<br>`extension.js#12625068` |
| **11** | Kiro 运行态可靠检测 | **决议：Windows 命名互斥量 `OpenMutexW` + 进程名快照**。<br>Windows 下调用 `OpenMutexW(SYNCHRONIZE, FALSE, L"kiro")` 耗时 <0.01ms，句柄实测存在（384）；跨平台辅以 `Kiro.exe` / `Kiro` 进程名遍历。确保在注入补丁或改写配置文件时绝不产生文件占用竞争。 | `docs/p0/aux-surfaces.md`<br>`product.json#win32MutexName` |

---

## 2. 结论对后续阶段的直接架构指导

1. **`kiro-wire` (P1-2, P1-3)**：
   - 依赖 `docs/p0/stream/*.bin` 作为二进制反向与往返 (round-trip) 编解码测试套件。
   - 保活定时器配置为 20s 间隔。
2. **`gateway` (P1-4..P1-9)**：
   - 将 `amz-sdk-invocation-id` 设置为请求上下文与账本幂等键（唯一约束）。
   - 在 Axum 的 Request Handler 中以 `tokio::select!` 监听 client disconnect，触发上游 HTTP cancel。
   - `GenerateCompletions` 统一挂载 HTTP 429 降级处理器。
3. **`patch-engine` (P3)**：
   - Windows 下利用 `OpenMutexW` 实现热修前进程占用检查。
   - 采用 `settings.json` + `extension.js` 精准打标，提供 `.bak` 还原机制。
4. **`billing` (P1-8)**：
   - 积分虚拟化口径统一对接 `getUsageLimits` 的微积分换算。
