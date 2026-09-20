# 运营与部署工程补齐报告 · 2026-09-20

## 结论与边界

维持商业上线 **No-Go**，本轮只是离线部署工程补齐，不是生产复验或放行授权。

依据 `2026-09-20-commercial-readiness.md`、`2026-09-19-release-ops.md` 及当前工作树工具。原审计的生产状态仅引用为历史证据，本轮没有连接生产、触发 CI、调用付费上游、操作真实 Kiro、修改证书/系统代理或执行部署/恢复。没有读取真实管理员凭据、主密钥或配置外部存储。未回滚、提交或暂存既有脏改动。

本轮写入范围：`deploy/**`、`.github/workflows/**` 与本报告。业务财务、桌面及网关代码由其他工作项负责，本轮不改动。

## 完成项

### 1. CI 制品与实际源码提交关联

新增 `deploy/artifact_provenance.py`，纯 Python 标准库加 Git。Windows x64、macOS arm64/x64 构建 workflow 在制品上传前生成同名 `.provenance.json`，包含实际 checkout HEAD、CI run/attempt、平台架构、字节大小、SHA256、生成时间。

拒绝脏工作树（暂存、未暂存和未跟踪源码）、空文件、缺失文件和符号链接输入；输出采用排他创建，不覆盖已有映射。PR 的实际 checkout 可能是合并提交，不把 PR head 或环境变量当作实际源码提交。构建标识 `ci-<run>.<attempt>` 不等于应用内版本或已批准公开版本。

`signature_verification=not_performed`、`publication_approved=false`，不将哈希/元数据冒充代码签名或实机验收。CI 仍为未签名候选构建，不自动发布。本次工作树本就有大量脏改动，未给它生成“干净提交构建”的虚假凭据。

新增 `deploy/test_artifact_provenance.py`，并加入主 CI。`deploy/NATIVE-PUBLISH.md` 补充最终签名后重算映射、重新验收与关联归档要求。

### 2. 在线备份包离线完整性检查与清理修复

现有 `server-backup.py` 的过期清理原先仅判断 manifest 是否存在，然后删除整个包，可能自动删除未完成或损坏的包，与“只清理完整包”的约定不符。

新增共享 `verify_bundle()`：核对 completed/version、manifest 文件名关系、快照与 anchor 摘要、版本与 sequence、anchor 校验值、代际文件内容、checksum sidecar；限制文件名并拒绝文件符号链接。过期清理仅删除通过检查的包；异常/缺件/畸形包保留，输出不含敏感数据的人工核查提示。保留异常包可能增加磁盘占用，必须监控日志和容量。

新增只读离线入口：

```text
python -B deploy/backup/verify-bundle.py <已取回的在线包manifest绝对路径>
```

它不联网、不读密钥、不执行 Docker、不写恢复目录。成功结果明确 `verification=byte-integrity-only`、`restore_verified=false`。适用范围是当前在线目录包，不取代冷备份/旧格式验证。哈希不是来源真实性认证；AEAD、账本不变量与真实恢复仍须真实 Rust verifier 和正确密钥。

新增 4 个备份回归测试方法，覆盖成功但不冒充恢复验收、每类缺失组件、路径穿越/状态/摘要/内容损坏、清理保留异常包。原有合法过期包删除测试继续保留。

`deploy/backup/README.md` 补充取回检查、告警及真实离机恢复所需流程。未虚构 S3/restic/rclone 端点、身份或成功上传记录。

### 3. 运营工具核查

- 服务端 `release_candidate.py` 已有 archive/candidate/configuration 摘要、部署锁与 release-state，不另造平行发布器，也未执行任何远程方法。
- `publish_native_windows.py` 已将操作员批准绑定到版本/平台/架构/摘要/大小，保护不可变历史文件；原发布门禁保持不变，provenance 不能替代批准。
- `publish_pricing.py` 已明确面值不是销售收入、套餐标价不是付款回执，未擅改其业务参数或执行发布。
- 在线备份已有同步、anchor 捕获、原子包发布；冷恢复已有停服门禁与真实 verifier 要求。本轮没有把它们的存在当作完成灾备演练。

## 财务口径核查

`deploy/pricing_policy.json` 四档为 1000/30、2000/55、5000/130、10000/250（积分/元），对应每积分 0.03、0.0275、0.026、0.025 元。统一 0.03 面值会对应 30/60/150/300 元，显然不是四档真实销售额；历史审计中的生产 0.01 面值与本地设置不一致也不能用本地文件替代线上证据。

放行须由财务/运营提供订单或人工收款台账，至少关联发卡标识、收款凭证、币种、实际到账、优惠、退款、支付手续费、时间及对账状态。敏感凭证不得写入公开仓库。管理端应区分：套餐标价、面值估算、实际净收款、消耗对应收入、上游估算/已对账成本、未消耗余额及毛利；不能直接把净收款减当期 token 估算成本宣传为真实利润。收入确认规则需业务/财务确认，本轮不代替会计决策。

## 测试与未验证范围

- 源码映射离线回归：5/5 通过，使用临时 Git 仓库，不生成真实发行包。
- 原生发布离线回归：6/6 通过，无 SSH 导入/连接。
- Python 备份回归：`python -B -m unittest discover -s deploy/backup -p test_server_backup.py -v` 共 25 项，24 通过、1 跳过（POSIX 文件权限/no-follow 检查）。本轮新加的 4 个测试方法均通过。
- 两个 workflow 经 PyYAML 解析通过；离线 verifier CLI 帮助正常，缺失输入返回退出码 1；检查结束无本次遗留 Bash 测试进程。未实际触发 CI。
- 5 个新增/修改 Python 文件 AST 语法检查通过；两个备份 shell 脚本 `bash -n` 通过；限定范围 `git diff --check` 通过（仅 LF/CRLF 提示）。
- 全备份 discovery 尝试在 Windows Git Bash 冷备份夹具出现挂起/错误，本轮已终止该次测试及其识别到的子进程，**未完成，不计为通过**。没有修改原有冷备份/恢复 shell 或其测试。需在隔离 Linux CI 重跑全套，不能用 Python 包校验替代。
- 未运行 GitHub Actions 实际构建、完整 Rust/前端测试、真实 Windows 签名验证、macOS 构建/签名/公证或真实网关恢复。Windows 路径测试中 POSIX 所有者/目录 fsync 使用原有 mock，不能据此认定 Linux 权限已验收。

## 外部门槛与验收证据

| 门槛 | 所需资源或动作 | 放行证据 |
| --- | --- | --- |
| 生产版本统一 | 授权发布负责人、已审查固定源码、生产只读核验窗口 | release-state、镜像 digest、前后端制品摘要、配置 revision、公开版本、CI 映射和回滚对象一一关联；不得追认未知历史 EXE 的源码 |
| 实收/估算分离 | 真实订单/收款与退款记录、发卡关联、财务口径批准 | 套餐逐档对账、退款/优惠/手续费测试、真实/估算标识、未消耗余额核对；不得靠改单一面值完成 |
| 离机灾备 | 实际外部存储与身份、加密和保留策略、独立主密钥托管、空白隔离 Linux 主机 | 从另一机器取回、正确密钥可恢复、错误密钥拒绝、真实 verifier 通过、余额/预留/pending/令牌/配置/审计核对、测得 RPO/RTO、告警演练和责任人签字 |
| Windows 发行 | 合法签名身份/安全私钥服务、时间戳服务、干净 Windows 实机 | 最终 EXE Authenticode 验证、发布者与证书链、时间戳、最终摘要映射、新用户安装/激活/恢复/换机闭环；当前 unsigned 候选不能等同正式发行 |
| macOS 发行 | Apple Developer ID、受保护凭据、公证服务、arm64/x64 真机或适当实机覆盖 | codesign/Gatekeeper、公证及 stapling 验证、实际 Kiro 布局与接入/恢复、最终包摘要、公开 manifest；仅 CI tar 包不算通过 |
| 商业闭环与容量 | 授权测试账号/卡、真实上游预算、目标并发与账本规模、观测系统 | 下载到对话扣费及两端积分一致、恢复/换机、pending/上游/存储/备份陈旧告警、负载与长周期数据；本轮不执行 |

历史共享凭据轮换仍由授权运维在受控窗口执行，不在本任务操作。原审计业务缺陷须由对应责任项复验，本报告不替其他代理的工作签署通过。
