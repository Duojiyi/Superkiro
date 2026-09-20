# Kiro BYOK 平台 — 总路线图与 TODO

> 这是**阶段级路线图**，不是逐任务的实施计划。每个阶段进入实施前，须按 Superpowers `writing-plans` 另出一份带 TDD 步骤与精确文件路径的实施计划（`docs/superpowers/plans/YYYY-MM-DD-<phase>.md`）。
>
> 设计基线：[`docs/superpowers/specs/2026-09-09-kiro-byok-design.md`](../specs/2026-09-09-kiro-byok-design.md)

## 全局约束（每个阶段隐含遵守）

- 服务端 Rust (axum + tokio + reqwest + sqlx)；桌面端 Tauri v2；管理面板 React + TS + Vite + Tailwind。
- 数据库 PostgreSQL 16；对象存储用于备份。
- 网关必须真实 TLS 证书（Caddy 自动签发）。
- 不记录对话正文与 API key；只记元数据。
- 所有 `【待P0验证】` 假设在 P0 未锁定前不得进入编码。
- 提交信息只描述产品变更与验证，不提内部流程工具。

## 仓库结构（目标）

```
kiro-byok/
  Cargo.toml                  # workspace
  crates/
    kiro-wire/                # Kiro/AWS Q 协议类型 + event-stream 编解码（纯库）
    gateway/                  # 冒充 Kiro 后端门面、鉴权、翻译器、provider 适配器、计量钩子
    billing/                  # 卡密/用户/分组/账本/配额/设备/rate-card（sqlx）
    patch-engine/             # Kiro 检测、版本识别、env 注入、补丁配方、备份还原
  apps/
    admin-ui/                 # React 管理面板
    desktop/                  # Tauri 客户端
  deploy/                     # docker-compose、Caddyfile、迁移、备份脚本
  docs/superpowers/specs/     # 设计文档
  docs/superpowers/plans/     # 实施计划
```

---

## 阶段 0 — 阻塞决策（写 P0 计划前）

- [x] 用户拍板：**无**分销层级（两层租户）
- [x] 用户拍板：UI 定制边界 = 有限钩子（模型列表 + 套餐/积分虚拟化）
- [x] 用户拍板：仅 IDE、不做 CLI
- [x] 用户拍板：卡密**从首次激活**计时
- [x] 用户要求：可扩展性为一等目标（spec §15）
- [x] 用户复核设计文档并批准

## 阶段 P0 — 探针（1–2 天，抛弃型代码）

目标：用真实报文锁定所有 `【待P0验证】` 假设，产出 kiro-wire 测试语料。

- [x] 出 P0 实施计划（writing-plans）
- [x] stub 服务器：记录一切入站请求（方法/路径/头/体/二进制帧原样落盘）
- [x] env 改道实验：`AWS_ENDPOINT_URL`* + `KIRO_AUTH_PORTAL_URL` 覆盖面实测；列出哪些调用**没**被改道
- [x] 持久性方案拍板：快捷方式劫持 vs 补丁兜底
- [x] 抓真实登录：定位 1.0.x 登录态存储位置与文件 schema；确认 Kiro 是否校验 token 签发方
- [x] 抓 `ListAvailableModels` / `getUsageLimits` / `listAvailableSubscriptions` / `ListAvailableProfiles` 的 1.0.x 真实请求与官方响应形状（可先让 Kiro 直连官方抓一份）
- [x] 抓含工具调用与图片的 `generateAssistantResponse` 完整二进制 event-stream（作为编解码一致性语料）
- [x] 确认 `GenerateCompletions` 在关闭设置后是否仍有调用；确认"额度上限"错误的期望响应形状
- [x] 验证 `KIRO_DISABLE_RECAP` / `KIRO_DISABLE_SESSION_TITLE_LLM` 生效
- [x] 产出：`docs/p0/` 报文样本 + 差异清单 + 决策记录；据此修订 spec（阶段门已过：见 `docs/audits/p0/PHASE-GATE.md`）

## 阶段 P1 — 网关核心（最小可用闭环）

- [ ] 出 P1 实施计划
- [ ] 初始化 monorepo（Cargo workspace、crates、apps、deploy、CI）
- [ ] `kiro-wire`：移植 ZyphrZero `parser/**`（frame/header/crc/decoder）与 `model/**`；新增**编码器**；用 P0 语料做 round-trip 一致性测试
- [ ] `gateway`：冒充门面路由（§4.2 全部端点，`GenerateCompletions` 返回结构良好的额度上限错误）；按**接口处理器注册表**组织（spec §15.1），方便后续加冒充面
- [ ] **套餐虚拟化**（spec §1.4）：`listAvailableSubscriptions`/`getUsageLimits`/`ListAvailableModels` 按分组的 `virtual_plan_name`/`virtual_usage_limit`/`model_catalog` 应答；验收含"免费真实账号显示 PRO 套餐与模型"
- [ ] provider 抽象为 trait（spec §15.2），OpenAI/Anthropic 两个实现登记到注册表
- [ ] 翻译器：Kiro ↔ OpenAI、Kiro ↔ Anthropic（工具名缩短还原、孤立 tool 修复、长 description 挪 system、图片压缩、thinking/reasoning、stop_reason）
- [ ] provider 适配器：OpenAI 兼容、Anthropic 兼容（流式）
- [ ] event-stream 流式输出：assistantResponse/toolUse/reasoning/metadata/contextUsage
- [ ] contextUsage 百分比：接模型窗口预设，used=tiktoken 估算
- [ ] 单卡密硬编码鉴权（Bearer 校验）
- [ ] Postgres 基础表 + 预留/结算账本雏形（真实 usage 扣费）
- [ ] 验收：本机 Kiro 经 DeepSeek/Claude 完成含文件读写工具的多轮对话；积分与模型列表在原生 UI 正确显示；P0 语料帧一致性测试通过

## 阶段 P2 — 计费与后台

- [ ] 出 P2 实施计划
- [ ] `billing`：卡密（`card_template` 模板/批量生成/导出/**激活即计时**/换绑/冻结封禁/备注/批量操作/续费码）
- [ ] 用户/分组（模型目录 + provider 绑定 shared/dedicated + 虚拟套餐名/额度 + **按分组模型可见性策略**）
- [ ] **套餐即多杠杆**：一个分组打包模型集/套餐名/虚拟额度/倍率/并发/设备上限，admin 改即生效（数据驱动，spec §15.3）
- [ ] rate-card 版本化 + 缓存 token 分级计费 + 失败退费语义 + 三种定价模式
- [ ] **积分定价体系**（spec §14.10）：积分面值/汇率锚点、三级倍率、定价工作台（所见即所得定价表/模拟器/套餐校准器）、`getUsageLimits` 积分口径
- [ ] 人工调账走账本 + 未激活卡密作废回收 + 容量护栏 + 按卡密上游并发配额 + 用量异常告警（spec §14.9）
- [ ] 预留/结算并发原子性；配额（日/月/并发/设备数）
- [ ] provider 治理：多 Key 加权轮询 + 冷却 + 故障转移 + 连通性测试与基准 + 模型上下文预设库
- [ ] 可观测：request_trace、按日聚合、成本 vs 收入对账、健康大盘、导出、数据保留策略
- [ ] 安全：provider key 加密（KMS/age）、多租户 RLS、admin 账号 + 2FA + 审计日志、每卡密限流、请求体上限、上游并发闸门
- [ ] 运维：备份 + PITR
- [ ] `admin-ui`：概览/卡密/用户分组/供应商/模型映射/用量报表/请求日志/对账
## 阶段 P3 — 桌面客户端

- [ ] 出 P3 实施计划
- [ ] 登录 + 设备指纹绑定 + 短时 token 换取
- [ ] `patch-engine`：Kiro 检测（Win/mac/Linux）+ 版本识别 + env 注入接管 + 补丁配方兜底（MARKER/备份/dry-run/restore/升级重打）
- [ ] 持久性方案落地（P0 结论）；macOS 只碰未签名扩展
- [ ] settings.json 安全合并（禁用 recap/title/autocomplete、update.mode=none）+ 精确回退
- [ ] token 写入 + Kiro 进程重启
- [ ] Doctor 体检 + 一键修复；接管状态可视（已接管/未接管/离线/维护/版本不匹配）
- [ ] 一键恢复官方直连（精确还原，无残留）
- [ ] 版本协商握手 + 健康信标上报
- [ ] 用量/余额/到期、模型偏好、离线宽限、公告、i18n、客户端自动更新、托盘
- [ ] 服务条款/免责首启展示

## 阶段 P4 — 增强

- [ ] 出 P4 实施计划
- [ ] 服务端下发补丁配方与 UI 覆盖（品牌/公告/隐藏改名模型/积分标签）
- [ ] 视觉降级 (Vision Fallback)
- [ ] `/mcp` web search 接自有搜索源
- [ ] 通知渠道（邮件/Telegram/Webhook）
- [ ] 发卡平台对接 API/Webhook
- [ ] 一键导入供应商
- [ ] 评估并（可选）冒充 autocomplete
- [ ] 分组级系统提示词注入（`system_prompt_prefix`）
- [ ] 客户端白标（名称/图标/主题/官网配置驱动）
- [ ] Web 自助激活门户（余额/到期/换绑，可选）
- [ ] 模型别名与降级链（主上游挂 → 备选模型）

## 阶段 P5 — 规模化

- [ ] 出 P5 实施计划
- [ ] Redis 分布式限流与计数
- [ ] 多实例部署 + 只读副本
- [ ] 模型灰度/AB
- [ ] 备份恢复演练

---

## 执行方式（阶段计划就绪后）

每个阶段的实施计划完成后，按 `writing-plans` 的交接选项二选一：
1. 子代理逐任务并行（推荐）——每任务一个新子代理，任务间两段式评审。
2. 本会话内按检查点批量执行。