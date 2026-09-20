# 管理后台表单登录：受控配置迁移（本轮不执行）

## 发布阻断条件

候选发布脚本会从旧 release 复制 Caddyfile.ip / Compose 配置，并以 configuration_digest 保护旧 env。修改仓库文件不等于修改候选或生产配置。禁止绕过摘要校验、直接覆盖生产 env 或直接发布本次登录改动。

必须先单独审批并执行配置迁移，验证迁移后的有效配置与摘要，再进行应用发布。readiness/domain_acceptance 的变更由发布流程负责人维护；本文件不改变其执行逻辑。

## 所需配置

网关环境（标准 Compose 为 deploy/.env；IP/域名部署为 /etc/kiro-byok/gateway.env）：

```dotenv
ADMIN_BROWSER_LOGIN=true
ADMIN_ORIGIN=https://kiro.rent
ADMIN_PASSWORD_HASH='$2a$12$用实际bcrypt哈希替换此占位值'
ADMIN_KEY=保留现有独立高熵服务端签名密钥
```

- ADMIN_ORIGIN 必须是实际管理员入口的完整 HTTPS origin，不带路径、末尾斜杠或默认 :443。标准域名部署与 DOMAIN 对齐。一个部署只允许一个管理员 origin；兼容 IP 地址不作为第二个登录入口。
- ADMIN_BROWSER_LOGIN 只接受 true。缺失、false、origin/hash 错误均使生产启动失败；直接构造生产路由也拒绝 raw key，绝不回退旧 Basic 或 x-admin-key 模式。
- 密码仅服务端 bcrypt 验证，固定用户名 admin；选择高熵密码，不超过 72 个 UTF-8 字节。
- 哈希用单引号包裹，避免 Compose/.env 插值破坏 `$`。不要把 ADMIN_KEY、密码或哈希写入日志、聊天或提交 Git。
- 密码哈希只供 gateway 使用，从 caddy.env 移除旧 ADMIN_PASSWORD_HASH。Caddy 保留 DOMAIN / SERVER_IP 等路由配置，不再负责 Basic 登录。

## 生成哈希

在可信运维终端交互运行已安装的 Caddy（或批准的 caddy:2-alpine 容器）：

```sh
caddy hash-password --algorithm bcrypt
# 或 docker run --rm -it caddy:2-alpine caddy hash-password --algorithm bcrypt
```

让工具交互读取密码，不使用 --plaintext 参数把密码留在 shell 历史或进程参数中。将输出 bcrypt 哈希配置到 gateway.env，权限保持仅运维用户可读；可复用原有有效 bcrypt 哈希，但务必确认它对应管理员实际密码，而非环境变量插值后的损坏值。

## 迁移核对顺序

1. 安全备份旧 release 配置与 env，记录现有配置摘要，确定回滚责任人和窗口。
2. 在受控候选配置中将三项浏览器认证变量放入 gateway 环境，移除 Caddy Basic 块和 Caddy 专用哈希；标准 Compose 已显式要求三项变量，IP Compose 从 gateway.env 读取并由网关启动校验。
3. 用 docker compose config --quiet 校验插值（不要输出完整配置到日志）；校验 Caddy 配置。由发布负责人按现有机制生成、审批新的配置摘要，不绕过 configuration_digest。
4. 在隔离环境核验匿名 /admin/ 返回登录壳且无 WWW-Authenticate；匿名管理数据为 401；Cookie 登录 -> GET /api/v1/admin/session 取 csrfToken -> POST /cards/reveal 和 /session/revoke，退出后旧 Cookie 不可使用。
5. 验证缺失变量时启动失败、错误 origin/缺失 CSRF 返回 403、密码连续错误触发 429、Cookie Secure/HttpOnly/SameSite=Strict。健康探测及客户端接口不应携带管理员凭据。
6. 配置迁移验收并更新摘要后才发布应用；回滚必须同时考虑应用与配置配套，不能仅恢复允许 raw key 的旧网关并保留公开管理壳。

旧 backup.sh 的在线 x-admin-key 同步请求不再适用于生产浏览器认证；未迁移前采用经批准的停机/离线一致性备份，禁止开放 raw key 作为兼容后门。本轮不改备份或发布脚本。


## KEK 与备份保障（发布前置条件）

- 本次登录迁移不得同时替换、重新生成或删除 Master KEK。`set_master_kek` 不是卡密轮换迁移：外层快照即使能用新 KEK 读取，内层旧卡密文仍可能只能用旧 KEK 解密。禁止以启动成功或 `codeRecoverable=true` 作为轮换成功依据。
- 在具备经验证的全量卡密重加密、原子提交和回滚工具前，禁止直接替换 KEK。旧 KEK 不得在旧快照、备份及旧卡密文仍依赖它时销毁。
- 配置迁移前取得一致性数据快照及对应版本的配置备份；密钥在独立受控密钥库中备份，保留密钥与快照版本的对应关系。不得把 KEK、管理员密码、cookie 或恢复出的卡密放进仓库、普通验收报告或日志。
- 使用隔离环境演练恢复：验证快照可读、卡余额及状态一致，并对已加密卡执行 reveal 比对原始卡密；同时确认旧不可恢复卡仍明确不可恢复。仅校验备份文件存在或校验和不足以证明可恢复。
- 旧在线备份脚本未完成 cookie 认证迁移前，继续使用经批准的停机/离线一致性备份。未取得可恢复备份和配置回滚方案，不批准本次配置迁移。
- `domain_browser_acceptance.run` 包含发卡、设备绑定/解绑和封禁清理，属于生产写验收，只能在单独授权后执行。本轮仅运行离线契约检查，不运行该入口；真实浏览器验收还会验证表单登录、cookie、CSRF reveal、退出及旧 cookie 失效，不保存浏览器认证状态。


## 现存自动备份入口与停机一致性（发布阻断项）

- 必须盘点并处理实际调用 `deploy/backup/server-backup.py` 的定时任务及其服务器安装副本。该入口读取 `gateway.env`，沿用旧 `ADMIN_KEY` 并将 `ADMIN_BASE_URL` / `HEALTH_URL` 固定为 IP 地址，然后调用 `current/deploy/backup/backup.sh`；不能因仓库登录配置已更新就视为备份已迁移。
- 发布前，运维负责人必须确认现存 cron / systemd timer 等实际调度入口：要么先完成认证及域名入口迁移并验证备份与恢复成功，要么暂停相关调度，确认没有仍在运行的旧备份进程，并登记暂停时间、负责人、替代备份安排和恢复调度条件。仅补文档不代表任务已暂停；未确认暂停或迁移完成则阻断发布。不得忽略旧脚本认证失败、恢复 raw-key 后门或将失败任务标记为成功。
- 未迁移时只能按批准的维护窗口执行停机一致性备份：先停止接收新的业务及管理写请求，等待在途请求和结算完成，正常停止 gateway 及所有共享数据写入进程，确认退出且数据已持久化。在整个复制或快照期间保持所有写入方停止；不能只停备份任务、只摘健康检查或在服务运行时直接复制数据目录。
- 停机后备份同一一致性时点的完整持久化数据及匹配的 release、配置与摘要，并按上节要求独立保管对应 KEK。确认备份完整可读、完成隔离恢复验证并记录停机区间后，按批准流程恢复服务；若正常停机、持久化或备份失败，应中止迁移并按回滚预案处理，不把不一致副本作为发布保障。
- 自动调度只能在新入口认证、实际域名/TLS 校验、失败告警及恢复验证均验收后恢复。暂停期间持续按批准的停机备份计划保障数据，不得无限期处于无有效备份状态。本轮仅补充要求，不修改或执行备份脚本，不操作服务器定时任务。
