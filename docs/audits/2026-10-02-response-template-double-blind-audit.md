# 响应模板多候选功能与项目审计收尾（2026-10-02）

## 结论与边界

多候选功能完成本地实现与回归：同一时间段可添加多条消息候选并随机或指定其一；每个模型变体可添加多份 HTML/路径候选并随机或指定其一。实际代码、消息选择索引进入收据，恢复不会重新选择代码或重复收费。

本报告合并原对话 `01a0fac8-62b6-7a83-bf17-f86b714939c1` 的两路独立阶段性审阅，并记录本轮主审复核与动态验证。不是重新执行的两路独立全仓审计，也不是安全认证、真实 Kiro 验收或生产发布批准。

- 基线 HEAD：`0831837`；验证对象包含当前未提交工作树，不能把结果直接归属于该提交。
- 环境：Windows、本地 fixture 与测试状态；不使用生产卡密、收费或线上发布操作。
- 原有 `pelican-bicycle.html` 删除、`apps/admin-ui/visual-check/` 和 `docs/audits/2026-09-30-double-blind-audit.md` 均保留。
- 本轮不提交、推送、部署或改动生产配置。下列既有归档风险作为审计发现保留，未扩大模板改动去重构归档系统。

## 独立审阅与复核覆盖

| 路线 | 原独立审阅范围 | 本轮收尾 |
| --- | --- | --- |
| A：数据、计费、安全、恢复 | Billing 引擎、模板、卡片、预约、账本、运行时设置；Gateway 认证、归档、管理/Portal 入口；备份入口 | 核对归档真实调用链、卡维度命名空间、幂等门禁、提交顺序与加密启动策略；补看在线备份捕获/发布/清理/认证及恢复校验入口 |
| B：UI、Gateway、CI、发布 | 模板编辑器、规则校验、协议、收费收据、重放、桌面 UI、CI/部署关键路径 | 补候选 UI 交互与持久化回归，执行 workspace、Clippy、管理台/桌面测试与构建，核对 unsigned 构件边界 |

原两路结论产生时未互相参考、未读取既有审计报告；本轮主审已经读取它们，因此本轮追加检查不宣称“盲审”。覆盖项目关键链路而非逐文件穷尽验证；所有生产环境、硬件与平台边界见末节。

## 已实现及修复

1. Billing 新增向后兼容的候选数组与选择索引：每变体最多 32 份代码、每 slot 最多 16 条文案，继续执行安全路径、非空候选、大小、时间窗口与索引边界校验。
2. Gateway 在一次请求内固定所选代码，工具 schema、下发、收据和计费账单使用一致路径与内容；每个 slot 仅发送一条选中文案。
3. 重放从持久化收据恢复同一工具指令，不重新随机、不额外扣费；恢复提示不是重新播放整条历史时间线。消息选择索引用于保留原始选择证据。
4. 管理台可增删、编辑候选并切换随机/指定。删除前置候选会调整索引，删除当前选择会回到随机模式。保存、刷新、冲突恢复沿用既有版本控制。
5. 原审阅发现的 `state_growth_test.rs` 错误结构体字段已修复，相关测试及全量测试通过。
6. 本轮修复候选文案删除按钮嵌在表单 label 内的问题，避免按钮文字污染文本框标签；同步更新被“候选 1”新标签影响的浏览器测试定位。
7. 新增真实无头浏览器回归：两份额外代码/消息、指定第三条、删除前置候选后的索引调整、保存刷新恢复、随机发布、删除已选项。
8. 加强 Gateway 回归：选中第三份代码的账单路径、保存到文件后新引擎恢复的完整收据、恢复前后财务不变；32 次随机请求核对收据与输出，并逐次重试验证工具指令一致、总账条数不增加。

## 验证记录

日志保存在本机 `.acceptance/response-candidates-*.log`，不含生产内容，不作为 Git 提交组成部分。

| 检查 | 结果 | 日志后缀 |
| --- | --- | --- |
| `cargo test --workspace --locked --no-fail-fast` | 1102 passed，0 failed，4 ignored | `workspace.log` |
| 补强测试后的 `cargo test --locked -p gateway --test response_templates_test` | 29 passed，0 failed | `gateway.log` |
| `cargo fmt --all -- --check` | 通过 | `fmt.log` |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 通过，补强测试后重新执行 | `clippy.log` |
| `npm --prefix apps/admin-ui test` | 全部规则/contract 脚本通过 | `admin-tests.log` |
| `npm --prefix apps/admin-ui run build` | 通过；保留原有大 chunk 提示 | `admin-build.log` |
| `npm --prefix apps/admin-ui run test:browser` | auth-gate、response-templates、runtime-settings-browser 全部通过 | `admin-browser.log` |
| `npm --prefix apps/desktop-ui test` | 7 文件、191 tests passed | `desktop-tests.log` |
| `npm --prefix apps/desktop-ui run build` | 通过 | `desktop-build.log` |
| `python -B -m unittest discover -s deploy -p 'test_*.py' -v` | 128 tests，OK | `deploy-tests.log` |
| `python -B -m unittest discover -s deploy/backup -p 'test_*.py' -v` | 51 项：49 passed、2 skipped，0 failed | `backup-tests.log` |

全量 Rust 测试后只补强了 Gateway 测试断言并调整管理台标签；Gateway 定向测试、最终前端测试/构建及 Clippy 已覆盖这些后续改动。4 项 Rust ignore 都是已安装 Kiro/真实 bundle 的本机检查，未当成通过。

备份测试跳过的两项为 POSIX 恢复所有权、凭据权限与 no-follow 文件描述符检查；Windows 结果不能替代 Linux 对这两项的验证。最终 `git diff --check` 通过，仅有 Git 的 LF/CRLF 转换提示。

本机第一次浏览器执行因未安装项目内 Playwright 失败；复用已有 npm 缓存模块并通过 `PLAYWRIGHT_MODULE`/`CHROME_PATH` 指向本机无头 Chromium 后通过，没有修改 package manifest 或锁文件。该环境使用 Node 26.4.0、缓存 Playwright 1.64.0-alpha 与 headless-shell 1228，不等同于 CI 的 Node 22 / Playwright 1.61.0；提交后的原生 CI 仍需独立通过。

## 审计发现与更正

### A1：跨卡同 ID、并发串写的原结论不成立；重试归档仍待专项验证

原审阅只看 `crates/gateway/src/archive.rs`，据 `key_of(invocation_id)` 推断跨卡复用 ID 会串档。本轮追溯 `crates/gateway/src/facade/conversation.rs:274` 及归档调用发现：传入的是 `card_id:invocation_id`，并在归档前通过同键幂等门禁，同卡同时请求返回冲突。因此不能把“不同卡共用客户端 ID”或“同卡并发”列为已确认的跨卡泄露漏洞。

保留较窄的可靠性风险：`archive.rs:234` 对同键的多个请求文件采用目录枚举中第一个匹配项；若失败重试或进程重启形成多份同键历史文件，读取没有显式选最新 attempt。`keep_request`/`keep_reply` 的后台文件写入也不是一个事务。需要围绕失败重试、跨进程恢复、异步写入顺序构造专项复现，现阶段不宣称已证实串档或越权。截断哈希本身也不足以证明存在可利用碰撞。

### A2：中等级别的既有运维一致性风险——快照提交失败可遗留孤儿账本归档

位置：`crates/billing/src/engine.rs:6983` 的 `archive_ledger`，先 `write_atomic_bytes` 写归档，再 `commit_candidate_snapshot`；当前没有失败清理归档的路径。

条件为归档写入成功、后续快照提交失败。账本事务仍遵循候选快照提交语义，不能由此推断重复扣费或已丢账；可能产生无 receipt 引用的文件、重复占用空间和运维歧义。本轮静态确认写入顺序，不声称完成磁盘故障注入。

建议单独补充失败注入、receipt 引用扫描与清理设计。不能简单把归档改成“快照成功后才写”：那会留下快照已引用而归档尚不存在的更危险崩溃窗口。

### A3：配置依赖的加密边界——显式明文模式下归档也为明文

`archive_ledger` 在 Master KEK 缺失时保留明文路径。但 `crates/gateway/src/main.rs:197` 默认要求加密；需同时显式关闭 `REQUIRE_ENCRYPTED_SNAPSHOTS` 并开启 `ALLOW_PLAINTEXT_SNAPSHOTS` 才能在没有 KEK 时继续启动。

这与开发/兼容模式有关，不是默认生产路径的认证绕过。生产必须保持加密要求、独立保管 KEK、限制归档/备份权限，并检查部署配置；本轮未检查线上密钥或更改运行策略。

### B1：已修复的测试构建阻断

新增 `message_selections` 曾误写进 `ResponseTemplateVariant` 测试构造器，实际字段属于 `ResponseTemplateReceipt`。已修复，工作区全量 Rust 测试通过。

### B2：发布验证缺口，而非模板代码缺陷

`.github/workflows/desktop-build.yml` 明确生成 unsigned Windows/macOS 构件；构件溯源、更新清单签名与 Windows Authenticode / macOS Developer ID + notarization 不是同一项保证。现有打包通过不能代替平台签名、实际安装、升级或恢复验收。

### 恢复与备份补充

在线备份代码采用 HTTPS、禁止重定向、cookie/CSRF 认证，同步后按不可变 generation 与 anchor 双读校验捕获，再以临时目录和原子 rename 发布；清理仅处理格式正确且校验完整的过期包。恢复脚本在发布数据前调用 Rust verifier，并保留回滚代际。这些是静态/离线测试证据，不等于真实 Linux 灾备演练。

备份包不是密钥托管或历史归档迁移的替代品；首次模板历史迁移需要旧快照引用的全部历史归档。按 `docs/RESPONSE-TEMPLATES.md` 的升级预检准备文件，不能仅凭快照字节完整性验证宣称首次恢复必然成功。

## 发布前仍需完成的外部验收

- 在已提交且 CI 通过的版本上执行隔离 stage → promote；当前工作树未提交，未部署。
- 真实 Kiro 执行文件工具，验证多候选、长等待取消、断连后重试及实际写入；mock stream 测试不证明真实网络最后一个字节已送达。
- 在 Windows 原生安装/升级及物理 Mac 上完成接管/恢复；需要平台签名的发行渠道必须提供有效签名/公证证据。
- 验证生产格式快照、归档和独立密钥在隔离 Linux 的恢复，记录 RPO/RTO；故障注入、备份来源真实性及外部反回滚锚点属于剩余运维安全工作。
- 上线后要回滚旧程序时先验证新候选字段与收据的兼容性，禁止删除收据、清空去重历史或伪造迁移标志。

以上不影响将本轮多候选开发与本地回归作为已完成工作，但不得据此把整个项目标记为“无风险、生产验收 100%”。
