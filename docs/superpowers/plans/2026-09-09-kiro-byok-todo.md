# Kiro BYOK 平台 — 详细 TODO（含双盲审计协议）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **Spec:** [`docs/superpowers/specs/2026-09-09-kiro-byok-design.md`](../specs/2026-09-09-kiro-byok-design.md)（v0.3）。本 TODO 从 spec 推导，spec 是权威；两者冲突以 spec 为准并回改本文件。
>
> **Roadmap:** [`2026-09-09-kiro-byok-roadmap.md`](./2026-09-09-kiro-byok-roadmap.md)（阶段级）。本文件是任务级。
>
> **粒度说明**：P0/P1 已细化到可执行任务（每任务有明确产出、验证命令与审计门）；P2–P5 为任务清单（进入该阶段前按 writing-plans 再展开为含 TDD 步骤与代码的逐步骤计划）。

---

## 0. 全局约束（每个任务隐含遵守）

- 服务端 Rust（axum + tokio + reqwest + sqlx）；桌面端 Tauri v2；管理面板 React + TS + Vite + Tailwind。
- 数据库 PostgreSQL 16。积分全程整数微积分（1 credit = 1_000_000 micro）。
- 网关真实 TLS（Caddy）。admin 与 Kiro 门面分离监听。
- 不记对话正文与 API key；日志脱敏中间件。
- **未经真实报文证实的假设不进代码**（spec §17.5）。
- TDD：先写失败测试，再最小实现，再通过，再提交。
- 提交信息只描述产品变更与验证，不提内部流程或工具名。
- 依赖锁定文件提交；`cargo audit` / `cargo deny` 进 CI。

## 0.1 双盲审计协议（每个任务必过的门）

每个任务完成后、标记 `[x]` 前，执行三段式审计。**实现者与两位审计者必须是不同执行者**（子代理驱动时用三个不同子代理；单人执行时至少间隔一次上下文切换并按角色卡片自审）。

```
[实现者]  完成任务 → 提交"完成声明"：改了哪些文件、跑了哪些命令、命令输出摘要
        ↓
[审计 A：规格盲审]  只读 spec 对应章节 + 任务描述；不看完成声明、不看 diff。
                   先独立写下"这个任务的验收标准是什么"。
                   再看 diff，逐条对照自己写的标准打勾。
        ↓
[审计 B：对抗盲审]  只看 diff + 测试；不看 spec、不看审计 A 结论。
                   假设"这段代码是错的"，找：边界条件、并发、错误路径、资源泄漏、
                   安全（注入/越权/泄密）、与相邻模块接口漂移。
                   必须实际运行测试并粘贴输出；必须至少构造 1 个反例输入并记录结果。
        ↓
[汇合]   A、B 各自独立提交结论后再互看。分歧 → 实现者修 → 重过 A/B。
         两者 PASS + 验证命令输出已粘贴 → 任务 [x]。
```

**每个任务的审计记录写入** `docs/audits/<phase>/<task-id>.md`，模板见 §0.2。**没有审计记录文件的任务不算完成。**

## 0.2 审计记录模板

```markdown
# <task-id> <任务名>
## 完成声明（实现者）
- 文件：
- 命令与输出摘要：
## 审计 A（规格盲审）
- 独立写下的验收标准：
- 对照结果：PASS / FAIL（逐条）
## 审计 B（对抗盲审）
- 运行的测试与输出：
- 构造的反例输入与结果：
- 发现的问题：
- 结论：PASS / FAIL
## 汇合
- 分歧与处置：
- 最终：PASS（日期）
```

## 0.3 阶段门（Phase Gate）

每个阶段结束需额外一次**阶段级双盲审计**：审计 A 逐条核对该阶段 spec 覆盖度（能否为每条 spec 要求指出对应任务与测试）；审计 B 端到端跑该阶段验收场景并尝试破坏它。记录写入 `docs/audits/<phase>/PHASE-GATE.md`。未过阶段门不得开始下一阶段的实现（可以开始下一阶段的**计划**）。

---

## 1. 目标仓库结构

```
kiro-byok/
  Cargo.toml                  # workspace
  crates/
    kiro-wire/                # Kiro/AWS Q 协议类型 + event-stream 编解码（纯库，无 IO）
    gateway/                  # 冒充 Kiro 后端门面、鉴权、翻译器、provider 适配器、计量钩子
    billing/                  # 卡密/分组/账本/预留/配额/设备/rate-card（sqlx）
    patch-engine/             # Kiro 检测、版本识别、env 注入、补丁配方、备份还原
  apps/
    admin-ui/                 # React 管理面板
    desktop/                  # Tauri 客户端
  deploy/                     # docker-compose、Caddyfile、迁移、备份脚本
  docs/
    superpowers/specs/        # 设计文档
    superpowers/plans/        # 计划与 TODO
    audits/<phase>/           # 每任务审计记录 + 阶段门
    p0/                       # P0 报文样本与决策记录
```

---

## 2. 阶段 0 — 阻塞决策

- [x] 无分销层级（两层租户）
- [x] 定制边界 = 有限钩子（模型列表 + 套餐/积分虚拟化）
- [x] 仅 IDE，不做 CLI
- [x] 卡密从首次激活计时
- [x] 可扩展性为一等目标（spec §15）
- [x] **G0-1** 用户复核 spec v0.4 并批准 → 记录于 `docs/audits/p0/G0-1.md`

---

## 3. 阶段 P0 — 探针（抛弃型，1–2 天）

**目标**：用真实报文锁定 spec §16 全部 11 条未知数，产出 `kiro-wire` 的测试语料。所有产物进 `docs/p0/`。**代码标记为 throwaway，不合入主线 crate。**

**阶段验收**：`docs/p0/DECISIONS.md` 对 spec §16 的 11 条逐条给出结论 + 证据文件路径；`docs/p0/samples/` 含登录、模型、额度、订阅、profile、对话（含工具+图片）的原始报文。

### P0-1 stub 捕获服务器 [x] (见 `docs/audits/p0/P0-1.md`)

**Files:** `spikes/p0-stub/`（throwaway，不合入主线）
**产出**：监听 HTTPS（本地自签，仅探针期临时信任），把任意路径的请求（方法/路径/头/体，二进制体按 hex + base64 双写）原样落盘到 `docs/p0/samples/<seq>-<path>.txt`；对需要 Kiro 继续跑的接口，可配置转发真官方后端并同时录制官方响应。
**验收**：能捕获并回放一条请求；二进制体不被文本编码破坏。
**审计门**：A 核对"是否忠实记录原始字节"；B 构造含 `\0` 与超长体的请求验证不截断。

### P0-2 env 改道覆盖面实测 [x] (见 `docs/audits/p0/P0-2.md`)

**依赖**：P0-1
**做什么**：用启动器注入 `AWS_ENDPOINT_URL` 系列 + `KIRO_AUTH_PORTAL_URL`，把本机 Kiro 指向 stub；操作登录、拉模型、看额度、发一轮带工具的对话；统计哪些调用命中 stub、哪些仍打向官方域名（对照 stub 日志与系统级抓包）。
**产出**：`docs/p0/coverage.md` —— 每个接口面一行：命中/未命中 + 用的什么环境变量或需补补丁。
**验收**：明确回答 spec §16-1（env 覆盖面）。
**审计门**：A 对照 spec §2.1/§2.4 接口面清单是否逐个覆盖；B 检查是否有"看似命中实为官方直连"的假阳性（比较响应来源）。

### P0-3 接管持久性方案定夺 [x] (见 `docs/audits/p0/P0-3.md`)

**依赖**：P0-2
**做什么**：验证"双击原生图标"是否绕过 env；测试快捷方式劫持（Win 开始菜单/桌面 lnk、mac LSEnvironment/包装、Linux .desktop）与补丁兜底两条路的可行性与副作用。
**产出**：`docs/p0/persistence.md` —— 选定方案 + 各平台落地方式 + 回滚方式。
**验收**：回答 spec §16-2。
**审计门**：A 核对是否覆盖三平台；B 尝试"卸载后残留""Kiro 升级后失效"两种破坏场景。

### P0-4 抓真实登录态与回调 [x] (见 `docs/audits/p0/P0-4.md`)

**依赖**：P0-2
**做什么**：让 Kiro 走一次真实登录（对官方），录制门户跳转、回调端口与参数、写入的登录态文件位置与 schema（对比 0.x 的 `~/.aws/sso/cache/kiro-auth-token.json` 差异）；确认 `KIRO_MACHINE_TOKEN` 等新变量作用。
**产出**：`docs/p0/auth.md` + 样本文件（脱敏）。
**验收**：回答 spec §16-3、§16-5。
**审计门**：A 核对 spec §2.6/§4.1 的未知项是否全部有答；B 检查样本是否彻底脱敏（无真实 token 泄漏进仓库）。

### P0-5 抓管理面响应 schema [x] (见 `docs/audits/p0/P0-5.md`)

**依赖**：P0-4
**做什么**：录制官方 `ListAvailableModels` / `getUsageLimits` / `listAvailableSubscriptions` / `ListAvailableProfiles` 的 1.0.x 真实响应；标注每个字段是否必填、Kiro UI 用哪些字段渲染模型下拉与积分面板。
**产出**：`docs/p0/mgmt-schema.md` + 样本。
**验收**：回答 spec §16-6。
**审计门**：A 核对四个接口齐全；B 删字段做"最小必填集"实验，确认哪些字段缺失会导致 UI 崩。

### P0-6 抓对话 event-stream（含工具+图片） [x] (见 `docs/audits/p0/P0-6.md`)

**依赖**：P0-4
**做什么**：录制官方 `generateAssistantResponse` 的完整二进制 event-stream，覆盖：纯文本、含 `toolUseEvent`（工具入参分片 JSON）、含 `reasoningContentEvent`、含图片输入、`contextUsageEvent`/`metadataEvent`；抓 `sessionIdleTimeout` 与 keepalive 行为；观察 `amz-sdk-invocation-id` 在人为断网重试时是否稳定；观察点"停止"时客户端行为。
**产出**：`docs/p0/stream/` 原始帧 + `docs/p0/stream.md` 解读。作为 `kiro-wire` 的 round-trip 测试语料。
**验收**：回答 spec §16-4、§16-7、§16-8、§16-9。
**审计门**：A 核对上述事件类型齐全；B 用 `kiro-wire` 原型解析这些帧，任一帧解析失败即 FAIL。

### P0-7 autocomplete 关闭与运行态检测 [x] (见 `docs/audits/p0/P0-7.md`)

**依赖**：P0-2
**做什么**：验证 `KIRO_DISABLE_RECAP` / `KIRO_DISABLE_SESSION_TITLE_LLM` 与设置项 `kiroAgent.enableTabAutocomplete=false` 生效；确认关闭后是否仍有 `GenerateCompletions` 调用及其"额度上限"错误期望形状；确定 Kiro 运行态可靠检测法（`win32MutexName`/进程/锁文件）。
**产出**：`docs/p0/aux-surfaces.md`。
**验收**：回答 spec §16-10、§16-11。
**审计门**：A 核对关闭三辅助面均验证；B 尝试用户重新打开 autocomplete 后网关返回是否被 Kiro 优雅接受。

### P0-8 决策汇总 [x] (见 `docs/audits/p0/P0-8.md`)

**依赖**：P0-1..7
**做什么**：`docs/p0/DECISIONS.md` 对 spec §16 全部 11 条逐条结论 + 证据路径；回填 spec 中所有 `【待P0验证】`（去标记或改为已确认）。
**验收**：spec 中不再有阻塞 P1 的 `【待P0验证】`。
**审计门（阶段门）**：见 §0.3，记录 `docs/audits/p0/PHASE-GATE.md`。

---

## 4. 阶段 P1 — 网关核心（最小可用闭环）

**阶段验收**：本机 Kiro 经 DeepSeek/Claude 完成含文件读写工具的多轮对话；积分与模型列表在原生 UI 正确显示；`kiro-wire` 用 P0 语料通过 round-trip 一致性测试；长等待不断流（保活生效）；SDK 重试不双扣（幂等生效）。

### P1-1 monorepo 脚手架 [x] (见 `docs/audits/p1/P1-1.md`)

**Files:** `Cargo.toml`(workspace)、四个 `crates/*/Cargo.toml`、`.github/workflows/ci.yml`、`rust-toolchain.toml`、`deny.toml`
**做什么**：建 workspace 与四个空 crate；CI 跑 `cargo fmt --check`、`cargo clippy -D warnings`、`cargo test`、`cargo deny check`。
**验收**：`cargo test` 绿；CI 通过。
**审计门**：A 核对结构与 spec §3.2 一致；B 确认 CI 真的会因 clippy 警告失败（故意加一个警告验证）。

### P1-2 kiro-wire：帧解码（移植） [x] (见 `docs/audits/p1/P1-2.md`)

**Files:** `crates/kiro-wire/src/frame.rs`、`header.rs`、`crc.rs`、`decoder.rs`、`tests/`
**做什么**：移植 ZyphrZero `parser/**`（prelude/header/CRC32/四态解码器）；用 P0-6 真实帧做解码测试。
**验收**：P0 语料每帧解码成功；损坏帧走恢复路径不 panic。
**审计门**：A 对照 spec §4.4 帧格式；B 喂截断/错 CRC/超大长度帧，验证不 panic、不越界。

### P1-3 kiro-wire：事件模型 + 请求模型 [x] (见 `docs/audits/p1/P1-3.md`)

**Files:** `crates/kiro-wire/src/events/*.rs`、`requests/*.rs`
**做什么**：定义 `assistantResponse/toolUse/reasoning/metadata/contextUsage/metering` 事件与 `conversationState/userInputMessage/tools/toolResults/images` 请求类型（移植 + 按 P0 校准字段）。
**验收**：P0 样本反序列化字段无丢失。
**审计门**：A 对照 spec §4.3 字段清单；B 用缺字段/多字段样本验证 serde 容错。

### P1-4 kiro-wire：帧编码 + round-trip [x] (见 `docs/audits/p1/P1-4.md`)

**Files:** `crates/kiro-wire/src/encoder.rs`、`tests/roundtrip.rs`
**做什么**：实现事件→二进制帧编码（prelude/header/CRC）；round-trip：编码后再解码等于原值；并与 P0 官方帧逐字节比对关键结构。
**验收**：round-trip 全绿；工具事件分片 JSON 与官方结构一致。
**审计门（关键）**：A 对照 spec §4.4；B 用官方帧做"编码器输出 vs 官方字节"差异比对，非平凡差异即 FAIL。

### P1-5 gateway：接口处理器注册表 + 门面路由 [x] (见 `docs/audits/p1/P1-5.md`)

**Files:** `crates/gateway/src/facade/mod.rs`(注册表)、`facade/*.rs`(各端点 handler)、`main.rs`
**做什么**：按 spec §15.1 建统一 handler trait + 注册表；挂 spec §4.2 全部端点（对话先接桩）。
**验收**：各端点可路由；新增 handler 只需注册一行（写一个 dummy 验证）。
**审计门**：A 对照 §4.2 端点齐全；B 确认未注册路径返回结构化错误而非 500。

### P1-6 gateway：鉴权中间件（单卡密硬编码 + token_version） [x] (见 `docs/audits/p1/P1-6.md`)

**Files:** `crates/gateway/src/auth.rs`、`tests/`
**做什么**：校验 Bearer JWT、解析 `card_id/group_id/token_version`；`token_version` 与内存值比对；单卡密先硬编码。
**验收**：无/错/过期/吊销 token 均拒；合法放行。
**审计门**：A 对照 spec §4.1；B 构造改签名、改 `token_version`、过期 token 各一枚验证全被拒。

### P1-7 provider 适配器 trait + OpenAI/Anthropic 实现 [x] (见 `docs/audits/p1/P1-7.md`)

**Files:** `crates/gateway/src/provider/mod.rs`(trait)、`openai.rs`、`anthropic.rs`
**做什么**：按 spec §15.2 定义 `translate_request/parse_stream/usage`；实现两个 provider（流式）。
**验收**：wiremock 模拟上游，两 provider 都能发起流式并解析出 delta 与 usage。
**审计门**：A 对照 §15.2 接口；B 断流/超时/非 200/无 usage 四种上游异常各测一次。

### P1-8 翻译器：Kiro ↔ provider [x] (见 `docs/audits/p1/P1-8.md`)

**Files:** `crates/gateway/src/translate/{to_provider.rs,from_provider.rs,tools.rs,images.rs}`、`tests/`
**做什么**：conversationState→provider 请求与响应→事件；实现 spec §4.3 全部坑：工具名缩短还原、孤立 tool 修复、超长 description 挪 system、图片压缩、thinking/reasoning、stop_reason。
**验收**：P0 对话样本翻译后语义等价；工具往返配对正确。
**审计门（关键）**：A 对照 §4.3 六条逐条；B 构造孤立 tool_result、超长工具名、超大图片、空 thinking 各一例。

### P1-9 gateway：流式编排 + 保活 + 幂等 [x] (见 `docs/audits/p1/P1-9.md`)

**Files:** `crates/gateway/src/stream.rs`、`idempotency.rs`、`tests/`
**做什么**：把 provider 流编码成 Kiro event-stream 下发；长等待插保活帧（spec §4.6）；按 `invocation_id` 幂等去重（spec §4.7）。
**验收**：模拟"首 token 前等 60s"不断流；同一 `invocation_id` 并发两次只转发上游一次。
**审计门（关键）**：A 对照 §4.6/§4.7；B 并发重放 + 中途断连两个场景验证不双发、无泄漏。

### P1-10 虚拟化只读接口 [x] (见 `docs/audits/p1/P1-10.md`)

**Files:** `crates/gateway/src/facade/{models.rs,usage_limits.rs,subscriptions.rs,profiles.rs}`
**做什么**：按分组返回自定义模型列表、虚拟套餐名、虚拟额度、固定 profileArn（schema 用 P0-5）。
**验收（关键场景）**：免费真实 Kiro 账号登录后，Kiro 原生 UI 显示 PRO 套餐名与我们设定的模型列表和积分。
**审计门**：A 对照 spec §1.4/§4.2；B 用 P0-5 的"最小必填集"验证字段不缺导致 UI 崩。

### P1-11 billing 雏形：Postgres + 预留/结算 + 真实 usage 计费 [x] (见 `docs/audits/p1/P1-11.md`)

**Files:** `crates/billing/src/{schema.sql,card.rs,reservation.rs,ledger.rs}`、`migrations/`、`tests/`
**做什么**：建 spec §5 核心表；实现预留（§6.2 估算+拒绝）、结算（真实 usage §6.1）、失败/中断退费（§6.3）、幂等计费（§6.7）、整数微积分。
**验收**：一次对话正确预留→结算→写账本；同一 `invocation_id` 不双扣；余额不足被拒。
**审计门（关键）**：A 对照 spec §5/§6；B 并发 5 请求打同一卡密验证不击穿余额、无孤儿预留（触发 janitor）。

### P1-12 端到端接入本机 Kiro [x] (见 `docs/audits/p1/P1-12.md` 及 `docs/audits/p1/PHASE-GATE.md`)

**依赖**：P1-1..11 + P0 持久性方案
**做什么**：启动器注入 env + 写登录态，本机 Kiro 指向本地 gateway；跑含文件读写工具的多轮对话。
**验收**：见阶段验收全部满足。
**审计门（阶段门）**：§0.3，记录 `docs/audits/p1/PHASE-GATE.md`；B 必须真机操作并录屏/贴日志。

---

## 5. 阶段 P2 — 计费与后台（任务清单，进入前按 writing-plans 展开）

- [x] P2-1 卡密：`card_template` 模板 / 批量生成 / 导出 CSV-JSON / **激活即计时** / 高熵+哈希 (见 `docs/audits/p2/P2-1.md`)
- [x] P2-2 卡密运营：换绑解绑（次数+冷却）/ 冻结解冻封禁 / 备注 / 批量操作 / 续费码 (见 `docs/audits/p2/P2-2.md`)
- [x] P2-3 即时吊销：`token_version` 全链（封号/冻结/换绑触发自增） (见 `docs/audits/p2/P2-3.md`)
- [x] P2-4 分组与虚拟化：套餐名/虚拟额度/可见性/`system_prompt_prefix`/provider 绑定 shared|dedicated (见 `docs/audits/p2/P2-4.md`)
- [x] P2-5 rate-card 版本化 + 缓存 token 分级计费 + 成本侧账本 + **三种定价模式**（成本加成/固定单价/按次包干，spec §14.10.2）(见 `docs/audits/p2/P2-5.md`)
- [x] P2-5b **积分定价体系**：全局 `settings`（积分面值/汇率）+ 三级倍率叠乘 + 定价工作台（成本录入/所见即所得定价表/定价模拟器/套餐校准器/一键生效版本记录，spec §14.10.3）+ `getUsageLimits` 积分口径（§14.10.4）(见 `docs/audits/p2/P2-5b.md`)
- [x] P2-5c 人工调账走账本（`usage_ledger.kind=adjustment/topup` + operator + reason，不直接改余额）+ 未激活卡密作废回收（spec §14.9）(见 `docs/audits/p2/P2-5c.md`)
- [x] P2-6 配额：日/月上限、并发上限、设备数上限 + **按卡密上游并发配额**（公平使用）+ 容量护栏（快速失败 + Kiro 可识别重试信号）(见 `docs/audits/p2/P2-6.md`)
- [x] P2-7 provider 治理：`provider_key` 多 Key 加权轮询 + 冷却 + 故障转移 + 连通性测试与基准 (见 `docs/audits/p2/P2-7.md`)
- [x] P2-8 模型上下文预设库（驱动 contextUsage 与压缩阈值） (见 `docs/audits/p2/P2-8.md`)
- [x] P2-9 可观测：request_trace / 按日聚合 / 成本 vs 收入毛利看板 / 模型成本排行 / provider 健康大盘 / **用量异常告警（速率突增→告警+自动限速）** / 降级公告下发 / 导出 / 数据保留策略 (见 `docs/audits/p2/P2-9.md`)
- [x] P2-10 安全：provider key 加密 + **主密钥外置注入与轮换** / 多租户 RLS / 登录爆破防护 / 请求体上限 / admin 分离监听 (见 `docs/audits/p2/P2-10.md`)
- [x] P2-11 运维：备份 + PITR / 基础限流 / 上游三级看门狗 / 优雅停机 / `/healthz` + `/metrics` / DB 抖动隔离 (见 `docs/audits/p2/P2-11.md`)
- [x] P2-12 admin-ui：概览 / 卡密 / 分组 / 供应商 / 模型映射 / 用量报表 / 请求日志 / 对账 / admin 2FA + 审计日志 (见 `docs/audits/p2/P2-12.md`)
- [x] P2-G 阶段门 (见 `docs/audits/p2/PHASE-GATE.md`)

## 6. 阶段 P3 — 桌面客户端（任务清单）

- [x] P3-1 登录 + 设备指纹绑定 + 短时 token；完整登录态握手（spec §4.1）(见 `docs/audits/p3/P3-1.md`)
- [x] P3-2 patch-engine：Kiro 检测（Win/mac/Linux）+ 版本识别 + 运行态检测 + 单实例锁 (见 `docs/audits/p3/P3-2.md`)
- [x] P3-3 接管：env 注入（含关闭 recap/title/autocomplete）+ 持久性方案（P0 定）+ 补丁兜底（MARKER/备份/dry-run/restore/升级重打）(见 `docs/audits/p3/P3-3.md`)
- [x] P3-4 接管前可逆快照 + 一键恢复官方直连（精确回滚，无残留）(见 `docs/audits/p3/P3-4.md`)
- [x] P3-5 settings.json 安全合并（只碰我们管理的键，update.mode=none）(见 `docs/audits/p3/P3-5.md`)
- [x] P3-6 写登录态 + Kiro 进程重启（运行中禁改文件的交互）(见 `docs/audits/p3/P3-6.md`)
- [x] P3-7 Doctor 体检 + 一键修复；接管状态可视（已接管/未接管/离线/维护/版本不匹配）(见 `docs/audits/p3/P3-7.md`)
- [x] P3-8 版本协商握手 + 健康信标上报 (见 `docs/audits/p3/P3-8.md`)
- [x] P3-9 用量/余额/到期、模型偏好、离线宽限、公告、i18n、客户端自动更新、托盘 (见 `docs/audits/p3/P3-9.md`)
- [x] P3-10 Windows 用户级安装免 UAC + 发布二进制**代码签名**；mac 只碰未签名扩展；首启一句话免责提示（不做条款留痕）(见 `docs/audits/p3/P3-10.md`)
- [x] P3-G 阶段门（真机三平台验收）(见 `docs/audits/p3/PHASE-GATE.md`)

## 7. 阶段 P4 — 增强（任务清单）

- [x] P4-1 服务端下发补丁配方与 UI 覆盖（品牌/公告/隐藏改名模型/积分标签）(见 `docs/audits/p4/P4-1.md`)
- [x] P4-2 视觉降级 Vision Fallback (见 `docs/audits/p4/P4-2.md`)
- [x] P4-3 `/mcp` web search 接自有搜索源 (见 `docs/audits/p4/P4-3.md`)
- [x] P4-4 通知渠道（邮件/Telegram/Webhook：发放/余额不足/provider 故障/维护公告）(见 `docs/audits/p4/P4-4.md`)
- [x] P4-5 发卡平台对接 API/Webhook（库存拉取 + 核销回调）(见 `docs/audits/p4/P4-5.md`)
- [x] P4-6 一键导入供应商（CC Switch / Cherry Studio）(见 `docs/audits/p4/P4-6.md`)
- [x] P4-7 分组级系统提示词注入 (见 `docs/audits/p4/P4-7.md`)
- [x] P4-8 客户端白标（名称/图标/主题/官网配置驱动）(见 `docs/audits/p4/P4-8.md`)
- [x] P4-9 Web 自助激活门户（余额/到期/换绑）(见 `docs/audits/p4/P4-9.md`)
- [x] P4-10 模型别名与降级链（主上游挂→备选模型）(见 `docs/audits/p4/P4-10.md`)
- [x] P4-11 评估并（可选）冒充 autocomplete (见 `docs/audits/p4/P4-11.md`)
- [x] P4-G 阶段门 (见 `docs/audits/p4/PHASE-GATE.md`)

## 8. 阶段 P5 — 规模化（任务清单）

- [ ] P5-1 Redis 分布式限流与计数
- [ ] P5-2 多实例部署 + 只读副本
- [ ] P5-3 模型灰度/AB
- [ ] P5-4 备份恢复演练
- [ ] P5-G 阶段门

---

## 9. spec ↔ TODO 交叉验证矩阵

> 每条 spec 要求都能指到至少一个任务。P0 结论回填后复核一次。

| spec 章节 | 覆盖任务 |
|---|---|
| §1.4 套餐虚拟化 | P1-10, P2-4 |
| §2.1–2.3 接管/持久性/完整性 | P0-2, P0-3, P3-3 |
| §2.4–2.5 接口面/关闭辅助面 | P0-7, P1-5, P3-3 |
| §2.6 登录态存储 | P0-4, P3-6 |
| §4.1 鉴权/吊销/登录流程/爆破防护 | P1-6, P2-3, P2-10, P3-1 |
| §4.2 端点清单 | P1-5, P1-10 |
| §4.3 翻译规则 | P1-8 |
| §4.4 event-stream 编解码 | P1-2, P1-3, P1-4 |
| §4.5 contextUsage | P1-8, P2-8 |
| §4.6 保活 | P0-6, P1-9 |
| §4.7 幂等 | P0-6, P1-9, P1-11 |
| §5 数据模型（含 settings/rate_card 定价字段/ledger kind） | P1-11, P2-5, P2-5b, P2-5c |
| §6 计费 | P1-11, P2-5 |
| §14.9 运营层（调账走账本/作废回收/容量护栏/公平使用/异常告警/降级公告） | P2-5c, P2-6, P2-9 |
| §14.10 积分定价体系（面值/汇率/三层价格/三模式/工作台/模拟器/校准器/积分口径） | P2-5, P2-5b, P1-10 |
| §7 安全 | P2-10, P3-10 |
| §8 运维 | P2-11 |
| §9 客户端 | P3-* |
| §10 版本兼容 | P3-8, P4-1 |
| §14 补充功能 | P2/P3/P4 各项 |
| §15 可扩展性 | P1-5, P1-7, P2-4 |
| §16 P0 未知数 | P0-1..8 |

---

## 10. 执行方式（P0 阶段门通过后）

按 writing-plans 交接：
1. 子代理逐任务并行（推荐）——每任务一个新子代理，任务间走 §0.1 双盲审计。
2. 本会话内按检查点批量执行。