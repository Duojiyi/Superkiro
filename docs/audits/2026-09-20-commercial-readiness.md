# Superkiro 全项目上线审计 · 2026-09-20

## 结论

**No-Go：不建议当前版本直接开放正式付费运营。可继续受控内测。**

本轮确认 8 项源码问题（2 P1、6 P2），另有生产版本、财务口径、灾备和发行验收缺口。没有确认 P0，不等于不存在漏洞。审计不是渗透测试认证，也不是所有平台/页面的完整实机验收。

## 方法与边界

三名独立代理分别审查计费/网关、管理/门户、桌面/跨平台，不共享初步结论；主审核验关键调用链、线上只读状态，并对两项 P1 发起交叉复核。这是独立交叉审查，不是严格实验双盲。

源码基准 HEAD：6d1902eb6556bcad1834acaa9d6dea91643f1173；工作树有大量未提交修改，结论针对工作树，不等同于该提交。线上 current：/opt/kiro-byok/releases/20260919T205821Z，容器镜像 kiro-byok:20260919T205821Z。

没有修改业务代码、部署、重启服务、操作真实卡密、调用付费上游、操作真实 Kiro/证书。线上仅查询状态、文件与下载，建立并撤销了管理员查询会话。报告不包含密码、卡密或密钥。

## 确认的问题

### A01 · P1 · 冻结/解冻破坏卡密生命周期

位置：crates/billing/src/engine.rs:2492、2526；crates/gateway/src/facade/admin.rs:1213；crates/billing/src/card.rs:211。

freeze 不限制来源状态，unfreeze 统一设为 Active。独立内存夹具确认：Unactivated→Frozen→Active 后 activated_at/valid_until 仍为 None；OAuth 登录成功而不初始化有效期。Banned、Voided 也可用该组合回到 Active。积分上限仍存在，但月卡时限可以失效。

这是合法管理员操作触发的状态机错误，不是匿名提权。线上 admin.rs 全文件及 freeze/unfreeze 函数与本地一致，生产源码存在同一缺陷；未在生产操作卡密复现。

修复验收：后端显式限制合法状态转换；禁止永久终态被普通冻结恢复；未激活卡不得绕过首次激活计时；单卡、批量、OAuth 与并发转换均需测试。

### A02 · P1 · 待结算缺少运行时恢复入口

位置：crates/gateway/src/main.rs:315；crates/billing/src/engine.rs:356、2123。

结算 intent 落盘后正式扣账失败，会保留 pending；新预留拒绝该卡，janitor 跳过 pending。retry_pending_settlement 存在但生产运行链没有调用，重启也不会自动结清。独立测试确认故障后 used=0/reserved=60，只有显式调用恢复 API 才完成180扣账。

线上 main.rs 与本地一致，retry_pending_settlement 函数一致；线上 janitor 同样跳过 pending。属于生产源码缺陷，不代表本轮发现已有真实卡片受影响。

修复验收：增加幂等、有界退避的自动恢复任务和可观测告警；覆盖落盘前后崩溃、恢复后重复执行、不可恢复数据错误；确保不重复扣费、不释放真实已消费预留。

### A03 · P2 · fallback 的实际成本按原请求模型记录

位置：crates/billing/src/engine.rs:2014、2038。

请求A、实际fallback到B时，用户收费和provider_cost都使用锁定的A价格版本。独立内存复现：A/B输入成本1/10元每百万tokens，B处理1000tokens，记录成本1000 micro-CNY而不是10000。用户继续按A收费可以是约定，实际成本不能共用该价格。

本地已确认；当前已发布模型配置没有fallback，因此不认定当前六模型直接受影响。启用fallback前须以实际provider/model解析成本并保留来源。

### A04 · P2 · 清理终态预留未同步内存

位置：crates/billing/src/engine.rs:2342、2351。

janitor从candidate快照删除超过7天的终态记录，但提交后的内存闭包没有移除prune_ids。独立复现run_janitor(700000)后export_snapshot仍包含旧记录。长期运行会反复处理并写回，增加内存、全量快照和锁竞争。

主审核对线上janitor：相比本地仅缺少active_reservations保护检查，此删除缺口也存在。需同时验证活跃长请求不可误回收，并让内存和已提交快照一致。

### A05 · P2 · 过期卡激活接口返回成功

位置：crates/gateway/src/facade/portal.rs:333、366；对照225。

Active但valid_until已过去的卡，不传device调用activate，得到success=true/status=active；query却返回expired。独立夹具已复现。未确认可绕过OAuth/扣费到期限制，当前portal-ui也未调用该入口，不夸大为现有网页激活故障。

线上portal.rs全文件与本地一致。应统一授权有效性检查与前端状态语义。

### A06 · P2 · 官方凭据恢复丢弃未知字段

位置：crates/patch-engine/src/token_storage.rs:37；desktop.rs:142、289。

原凭据被反序列化为固定六字段KiroAuthToken后保存，恢复重新序列化；未知JSON字段不保留。应保存受保护的原始数据，并在恢复时保证无损。字段丢失由源码确认，具体Kiro版本是否依赖额外字段尚未实机验证。

### A07 · P2 · 设置快照未明确保护权限

位置：crates/patch-engine/src/settings.rs:124；snapshot.rs:85、138。

完整settings原文进入快照，File::create没有0600限制。Unix/Mac umask022且其他用户可遍历目录时，原0600文件中的代理认证/扩展密钥可能通过0644快照暴露。需对目录、临时文件、最终文件和Windows ACL统一保护。静态确认条件性风险，不声称本机已泄露。

### A08 · P2 · Mac检测与预检路径不一致

位置：crates/patch-engine/src/detect.rs:264；patch.rs:505；desktop.rs:200。

检测接受MacOS/Kiro或MacOS/Electron，预检却固定MacOS/Kiro。只有Electron布局时会出现检测成功但接入失败。应使用已验证的installation.executable_path。规则矛盾已确认，目标真实Mac发行布局和接入后签名/Gatekeeper行为未验收。

## 上线运营缺口

1. **版本没有统一。** 生产commercial-config不返回settings，financials设置仍为credit_face_value_cny=0.01、usd_cny_rate=7.25；本地已有更新。固定积分/CNY成本不能因此判错，但财务估算不等于套餐真实收款。后台index与本地dist一致，不能据此认为后端也是最新版。
2. **真实销售账未闭环。** 四档售价格为30/55/130/250元，不能用统一积分面值准确反推各档实收。上线需订单或人工收款台账与发卡关联，明确优惠、退款、未消耗余额及毛利口径；估算不能展示成实际利润。
3. **灾备证据不足。** kiro-backup.timer已启用；最近service Result=success，时间2026-09-19 07:41:52 UTC，约16.5小时前；存在本机备份manifest。未完成离机备份、独立主密钥取回、空白环境恢复和RPO/RTO演练。本机备份不防整机丢失。没有证据不等同于断言外部系统不存在。
4. **公开发行范围有限。** manifest只有Windows x64，版本0.1.0-native.20260919.2，标记unsigned；公开exe可下载，12527104字节及SHA256均与manifest一致。未核验Authenticode；没有公开Mac条目。debug与发布文件摘要不同不能单独证明版本落后，须建立源码提交/构建/制品映射。
5. **尚未完成真实商业闭环验收。** 本轮未验证全新用户下载→激活→真实Kiro接入→模型对话→服务端扣费→两端积分一致→恢复→换机，也未进行生产付费调用、真实Mac、公证或故障恢复演练。不能承诺全部功能均已商用就绪。
6. **长周期与容量边界需明确。** 单机状态快照、单网关实例尚需按实际目标并发和账本规模压测；缓存TTL/长上下文不能宣传超出已配置和验收范围。监控需覆盖pending、上游失败、存储失败、备份陈旧，而非只监控healthz。
7. **发布前轮换共享过的凭据。** 历史协作中传递过服务器和上游凭据，建议上线前轮换并采用受保护配置；本轮不声称已遭泄露，也未自行更改凭据。

## 本轮通过的检查

- cargo test --locked -p billing -p gateway -p kiro-wire --no-fail-fast：382通过、0失败、0忽略；70组结果包含零用例doc-tests。日志：.acceptance/operations-audit-rust.log。
- 桌面前端vitest：106/106通过；管理员npm test合同/会话/超时检查通过。
- 备份Python测试：42项，40通过、2跳过；不是42项全部执行通过。
- 独立代理另跑billing定向9项、gateway stream resilience10项以及隔离复现，不与主套件简单累加。
- 生产gateway healthy；healthz200；匿名admin stats/cards401；/.env404。/admin/200是登录壳，不认定管理数据泄露。
- 生产仅公开80/443，gateway19820未映射主机；容器内存限制1GiB/pids256；配置凭据文件0600；磁盘使用44%，无遗留deployment.lock。
- 本轮未跑完整构建、浏览器逐页视觉回归、原生窗口自动化、依赖漏洞库扫描或负载测试；前端单测通过不代表1:1设计或所有交互验收通过。

## 建议放行顺序

1. 修复A01/A02，补故障注入与状态矩阵回归；排查现有异常状态但不自动篡改用户账。
2. 修复A03—A08，统一前后端生产候选版本，并为财务显示明确真实/估算来源。
3. 完成离机恢复演练、告警验证、凭据轮换以及Windows候选包真实闭环验收；Mac单独设置放行门槛。
4. 用固定源码版本生成制品、审计证据和回滚方案，先小规模灰度，再正式售卖。不要把测试用例数当作上线许可。
