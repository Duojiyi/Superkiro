# 项目双盲审计报告（2026-09-30）

## 结论与审计边界

**结论：两路独立静态审阅均未报告已确认的实质性缺陷，但当前工作树不应直接发布。** 主审复核确认格式门禁失败、发布输入尚未提交，并记录完整用量查询的功能限制及新增功能验收缺口。没有发现问题不等于证明没有问题，本报告不是第三方安全认证，也不是生产上线验收单。

- 项目：`D:/Desktop/kiro byok`。
- 审计从 2026-09-30 延续至本机时间 2026-10-01 00:10 后（Asia/Shanghai，UTC 仍为 2026-09-30）。
- 分支：`fix/final-audit`；HEAD：`a7442ac30b86645cbdc060f2ee3f6e8c87da6f3a`。
- HEAD 与本机 `origin/fix/final-audit` 跟踪引用相同；本轮未 fetch，因此不把它当成对 GitHub 当前远端状态的实时验证。
- 覆盖当前已跟踪源码、未提交差异及未跟踪产品源码，而非只审查 HEAD。
- 只新增本报告；没有修改业务代码、运行格式修复、部署、提交或推送，也未进行真实充值或消耗生产卡密。
- 不收录卡密、API 密钥、生产凭据、私钥和用户消息内容。

## 双盲方法与覆盖

两名审阅者在独立审阅阶段未共享彼此发现；主审收齐结论后交叉复核。两者都进行了只读静态审阅，没有运行测试、构建或线上验证；下方动态检查来自主审阶段，不能说成由两路分别验证通过。

| 审阅者 | 覆盖范围 | 独立结果 |
| --- | --- | --- |
| A / Fermat | 计费、预约与结算、卡密续费与用量、Gateway/认证/流式、桌面宿主/更新/恢复、UI、部署与 CI | 未确认具有完整可达调用链和实质影响的缺陷；保留故障恢复及平台行为风险 |
| B / Schrodinger | 桌面 React/Tauri、凭据与信任边界、更新链、Gateway/provider、计费持久化、Admin/Portal、部署与 CI | 未确认满足其报告标准的缺陷；明确缺少运行时及真实环境验证 |

覆盖主要产品模块，但不是穷举所有输入、依赖和环境组合。以下“已知功能限制”和“验证缺口”由主审整理，不冒充两路共同复现的漏洞。

## 按优先级汇总

P0/P1：本轮没有确认此级别的漏洞或业务故障；不排除未覆盖场景。P2/P3 为本项目整改优先级，不是安全漏洞评分。

| 编号 | 优先级 / 性质 | 发现及影响 | 证据与最小建议 |
| --- | --- | --- | --- |
| F01 | P2 / 已确认发布阻断 | `cargo fmt --all -- --check` 返回 1。当前差异会使 CI 格式门禁失败，Clippy 通过不能替代它。 | `.github/workflows/ci.yml:79`；`crates/billing/src/engine.rs:3820` 等。后续修复阶段执行格式化，检查差异，再重跑门禁。 |
| F02 | P2 / 已确认发布阻断 | 续费、用量、UI/窗口调整等仍在未提交工作树，包含未跟踪产品文件；不能据此宣称已经推送或上线。标准服务端发布脚本拒绝脏发布输入。 | `deploy/release_candidate.py:112-135`；`git status`。后续需先形成可追溯提交、通过 CI，再走标准发布与验收。 |
| F03 | P2 / 已确认功能限制 | 只要卡密统计区间中有已归档的用量账本，服务端就返回无完整统计，桌面整个完整用量面板变为不可用。未记录激活时间的旧卡虽有后端近 30 天回退，但也不满足桌面完整面板条件。 | `crates/billing/src/engine.rs:4406-4436`；`apps/desktop-ui/src/UsagePanel.tsx:9-20`；`crates/billing/tests/settled_usage_test.rs:136-139`。需补归档可查询统计或明确分开完整累计与有限区间视图，不能把局部数据标为自激活累计。 |
| F04 | P2 / 验证缺口 | 新增卡密续费有引擎测试，但没有找到贯穿 Gateway challenge、续费路由、持久化及桌面刷新的专用集成验收；现有发布验收也未显式覆盖本次新功能。 | `crates/billing/tests/renew_card_test.rs:15-70`；`crates/gateway/src/facade/portal.rs:657-728`；`deploy/accept_native_release.py:34-65`。补成功、重复/并发、失效源卡、失败后重试、重启恢复和状态刷新用例。 |
| F05 | P2 / 真实环境验证缺口 | 当前改动未完成真实 Windows 发行包的升级/回退及实际服务器的新功能验收；Kiro 长上下文与断流场景也未复测。页面测试通过不能替代这些行为。 | `crates/desktop-host/src/update.rs`、`crates/gateway/src/stream.rs` 和实际验收记录范围。按提交、服务端镜像、客户端包 hash 关联进行候选环境验收，避免直接用真实客户卡试验。 |
| F06 | P3 / 产品口径 | 每日统计使用 UTC，自中国用户视角会在北京时间 08:00 换日，而非本地午夜。UI 已明确说明 UTC，因此不是已确认的漏扣或计算错误。 | `crates/billing/src/settled_usage.rs:51-52`、`:74-87`；`apps/desktop-ui/src/UsagePanel.tsx:21`。明确是否继续使用 UTC；若改为中国自然日，要同时修改聚合和测试，不能只换标签。 |

### F01 涉及文件

本次格式检查输出至少涉及 `crates/billing/src/engine.rs`、`crates/billing/src/settled_usage.rs`、`crates/billing/tests/renew_card_test.rs`、`crates/desktop-host/src/backend.rs`、`crates/gateway/src/facade/portal.rs`。本轮只运行检查，没有执行会改写文件的 `cargo fmt`。

### F02 服务端与客户端必须一起交付

本次不是只有客户端样式改动。卡密转入续费在服务端新增 `renew_card` 请求分支与原子账本操作；客户端完整用量依赖新的激活时间与按模型每日字段。仅换客户端不能完成这些需求，必须验证 Gateway/billing 和客户端版本的配套兼容性。

`deploy/release_candidate.py:128-135` 会拒绝未提交/未跟踪发布输入，并要求提交属于 `origin/main` 且 CI 满足条件。当前分支名不等于提交一定不属于 main；本轮没有 fetch/验证该祖先关系，确定的阻断是脏发布输入。不要为赶上线绕过这一保护。

### F03 不等于少扣积分

归档场景返回不可用是避免输出伪造的完整统计，不代表卡密余额或账本扣费错误。此处检查的是客户可见查询能力，未重新逐条核对 Kimera 账单与我方扣费。24 小时的加密请求内容归档与计费用量账本归档是不同机制，不能混为一谈。

## 已复核的保护措施与非问题

- 续费要求源卡未激活、未用、未绑定设备、无有效期且积分为正；拒绝同卡转入。目标卡须已激活且处于允许状态，源卡作废与目标入账通过 candidate snapshot 一次性提交。见 `crates/billing/src/engine.rs:3820-3868`。
- 当前续费规则是有限期卡至少延至续费时间加 30 天，保留更晚原到期日，永久卡继续永久。UI 已说明，并非每次强制覆盖为恰好 30 天。见 `engine.rs:3839-3840`、`apps/desktop-ui/src/RenewCard.tsx:23`。
- 已有续费只消费一次、并发、快照恢复后对账、过期目标和永久目标测试，不能说续费完全没有测试。见 `crates/billing/tests/renew_card_test.rs`。
- 用量以已结算 Usage 账本聚合并去重，充值不计成模型消耗；桌面面板只展示积分，没有美元或 token 展示。见 `crates/billing/src/settled_usage.rs:58-80`、`apps/desktop-ui/src/UsagePanel.tsx:22-26`。
- 本地查询路径已支持只验证卡密而不启用连接，见 `apps/desktop-ui/src/App.tsx:109-114`。是否适配当前线上服务仍需联调。
- 顶部窗口配置 `maximizable: false` 已存在，但仍需原生 Windows 不同缩放/屏幕尺寸验收，不能由浏览器测试推定修复了所有原生窗口问题。
- 更新 manifest 有 Ed25519 验签、版本/hash/大小校验和试启动回退；发布脚本校验可执行文件版本 marker。见 `crates/desktop-host/src/update.rs:188-284`、`deploy/publish_native_windows.py:28-43`。
- `bundle.active: false` 本身不是已确认发行缺陷：Windows 使用 `scripts/build_desktop.py:30-47` 编译并复制便携原生 EXE，不依赖 Tauri installer bundling。开发包未设置正式版本时禁用自动更新也是设计行为。
- 安装更新前宿主获取共享 operation 锁，避免与本地变更操作并行。不能仅凭前端弹窗状态推定更新会中断正在执行的续费。见 `crates/desktop-host/src/backend.rs:245-267`、`crates/desktop-host/src/main.rs:299-313`。
- Gateway 存在请求层超时、provider/watchdog、流式保活及发送期限等不同边界；源码显示 10 MB 全局 body limit、300 秒请求层 timeout 及流式保活。不能把全部长上下文问题归结为一个等待时长。见 `crates/gateway/src/main.rs:401-403`、`crates/gateway/src/stream.rs:130-156`。
- 已有结算失败留待恢复、流式断开处理和管理员认证/CSRF保护代码；本轮没有形成已证实的绕过或重复扣费链路，但并不替代真实故障注入与外部账单对账。

## 验证记录

“上一阶段”指本轮任务延续前已有的验证记录，收尾未重复运行；“收尾复核”指本报告写入前直接取得的命令结果。

| 检查 | 来源 / 结果 | 边界 |
| --- | --- | --- |
| `cargo fmt --all -- --check` | 收尾复核，失败，退出码 1 | 只检查，没有改写文件 |
| `cargo clippy --workspace --exclude desktop-host --all-targets --locked -- -D warnings` | 收尾复核，通过，退出码 0 | 明确排除了 desktop-host；不能说桌面宿主 Clippy 也通过 |
| 桌面前端单元测试 | 上一阶段，189 项通过 | 不覆盖真实原生窗口与安装升级 |
| 管理后台测试、管理后台和桌面前端 production build | 上一阶段，通过 | 不是生产部署成功的证明 |
| `python -B -m unittest discover -s deploy -p "test_*.py" -v` | 上一阶段，98 项通过 | 部署脚本回归，不等于这些流程在生产实际运行 |
| `python -B apps/desktop-ui/verify_layout.py` | 上一阶段，35 个视口/状态组合通过 | 浏览器布局检查 |
| `python -B apps/desktop-ui/verify_browser.py` | 上一阶段，通过 | 不等同于 Kiro/Windows 原生端到端验证 |
| Rust 工作区测试中的认证与备份测试 | 上一阶段，受环境影响的认证测试补充 NO_PROXY 后通过；备份独立运行通过 | 不把曾失败的整次运行描述为一次全绿，也不把代理/资源竞争直接认定为业务缺陷 |
| Gitleaks、cargo-deny、cargo-audit | 静态确认 CI 有配置；本收尾没有重新运行 | 未验证最新依赖漏洞库，也不宣称本次秘密扫描通过 |
| 生产管理员真实登录、新续费/用量、原生安装/更新/回退、真实长上下文 | 未运行 | 仍须验收 |

环境备注：认证回归曾受本机代理影响，使用 `NO_PROXY=127.0.0.1,localhost,tokenrhythm.studio` 后通过；备份检查并发时超时、单独运行通过。应先隔离环境再定位，不能将它们当成已复现的服务端业务问题。

## 发布验收不足

`deploy/verify_candidate.py:41-46` 检查健康、静态页面一致性及带 Basic 参数的页面请求，不足以证明真实管理员 cookie 登录成功。更完整的 `deploy/accept_native_release.py:34-65` 覆盖会话 cookie、CSRF、一次性测试卡、真实模型请求扣费和重放拒绝，但本轮没有在线运行，而且没有显式覆盖新增的卡转卡续费与完整用量统计。

目前不能宣称服务端已更新、客户端更新包已发布、卡密续费已在线验收、没有漏扣积分，或当前 GitHub 内容等于全部本地修改。

## 建议下一轮执行顺序

1. 格式化相关 Rust 改动并审阅 diff；确认应纳入版本的新增产品文件，保留并单独处理无关工作树改动。
2. 明确归档后完整统计与旧卡的产品承诺，以及 UTC 日界线；按约定补功能或明确限制。
3. 补续费 HTTP 与桌面联调测试，验证失败/重试/并发、源卡作废、目标积分和有效期、重启对账，及无需连接即可查用量。
4. 运行质量门禁，形成可追溯提交并通过既定 CI；不要关闭发布前置检查。
5. 在受控环境同步部署服务端候选和正式标识客户端包，验收原生窗口、更新/回退、长上下文及新功能；通过后再按既定流程上线。

本轮到此仅完成审计与报告，不代表修复、发布、提交或推送完成。