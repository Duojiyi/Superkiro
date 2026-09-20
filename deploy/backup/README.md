# Superkiro 备份与恢复

账本是单机持久化状态与加密快照，不是数据库。备份必须包含不可变代际、锚点、校验和与提交清单；主密钥另行保管，不能和备份一起公开。

## 在线备份

生产使用 `server-backup.py`，通过 HTTPS 管理员账号登录、Cookie 和 CSRF 请求同步，再捕获已提交的不可变代际。禁止关闭 TLS 验证，禁止使用旧 `ADMIN_KEY` 或 `--force` 绕过。

默认部署根目录 `/opt/kiro-byok`，凭据 `/etc/kiro-byok/admin-access.json`，内容为 JSON `username`、`password`；开启 TOTP 时增加 `totpSecret`（Base32 种子，不是六位动态码）。凭据必须是 root 或执行用户所有、权限 0600 的普通文件；禁止提交版本控制、写入 shell 参数或日志。自动化任务持有第二因素种子并不等于真人第二因素隔离，应限制 root 访问，后续多管理员身份落地时给备份使用独立最小权限身份。

```bash
sudo ADMIN_ORIGIN=https://kiro.example.com python3 /opt/kiro-byok/deploy/backup/server-backup.py
```

返回成功只说明同步及文件完整性检查通过，不代表已完成解密恢复演练。每次备份原子发布到 `backups/server-backup_<time>_<uuid>/`。目录中的 `billing_state_*.json`（不含 generation 名称者）可作为 `restore.sh` 的输入。`RETENTION_DAYS` 控制本机完整备份目录的保留，默认 7 天；离机保留需要另行配置。

## 定时任务

服务样例 `superkiro-backup.service` 和 `superkiro-backup.timer` 与脚本一起维护。安装前调整路径和 ADMIN_ORIGIN，确认凭据权限、主机时间同步及一次手动备份成功，再安装并启用：

```bash
sudo install -d -m 0700 /opt/kiro-byok/backups
sudo install -m 0644 deploy/backup/superkiro-backup.service /etc/systemd/system/
sudo install -m 0644 deploy/backup/superkiro-backup.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now superkiro-backup.timer
```

任务只在成功后输出清单路径，失败返回非零状态且不输出凭据。TOTP 同一时间步被使用时会失败关闭，稍后重试；样例任务使用 systemd 的有界重试。监控失败状态和最新成功备份时间，不只检查 timer 是否启用。

## 冷备份

存在代际引用时以锚点指向的已提交代际为准，镜像缺失或落后不影响冷备份。先停止网关及所有其他写账本进程，再执行 `deploy/backup/backup.sh`。脚本直接检查实际容器状态（两份 Compose 默认容器名均为 kiro-gateway，定制时设置 GATEWAY_CONTAINER），只接受 exited / created；Docker 不可用、容器缺失或状态未知均拒绝。健康端点响应也是拒绝条件，不支持强制参数。整个备份/恢复期间不得同时启动部署或其他账本写入者。自定义 DATA_FILE/BACKUP_DIR 前确认指向同一部署；不可把网络探测失败当作停机证据。此停服检查针对容器部署，非容器部署需要独立验证方案，不能绕过。冷备份只清理目录根部自己生成的成套文件，不进入在线备份子目录；共享代际文件保留，容量清理需另行确认引用。

## 恢复

1. 停止网关和备份 timer，确认没有其他账本写入者。保留当前数据的离线副本。
2. 从完整备份目录恢复，提供正确的 `KIRO_MASTER_KEK`（通过受保护环境文件注入，不写历史命令）。
3. 调用 `deploy/backup/restore.sh /absolute/path/billing_state_<id>.json`。脚本核对 manifest、锚点和 SHA256，并在临时沙箱调用真实 Rust verifier 检查 AEAD、代际及账本不变量；没有 verifier 则失败关闭，不接受仅 Python 格式校验。

恢复禁止 `--force`。恢复文件保持 0600，归属默认与容器一致的 UID/GID 1000；自定义服务用户通过 `GATEWAY_UID` / `GATEWAY_GID` 显式设置。root 恢复使用 `setpriv` 验证实际网关身份能读文件且能写数据目录，权限不足在发布前失败。

4. 在隔离部署先核对卡数量、余额、预留、已消费刷新令牌、模型配置和审计记录，再恢复生产服务及 timer。

完整演练还需验证离机备份可读、密钥可取得、恢复时间满足要求。此仓库变更不代表生产 timer 已更新或真实灾备演练已通过。

## 离线检查取回的在线备份包

在任意隔离机器上运行 `python -B deploy/backup/verify-bundle.py /absolute/path/billing_state_<id>.manifest.json`。该命令不联网、不读主密钥、不调用 Docker、不写恢复目录；只验证本脚本生成的在线目录包，不替代冷备份/旧格式恢复验证。它交叉核对 manifest、快照、anchor、代际文件和 checksum，拒绝不完整包、路径穿越和文件符号链接。成功输出 `verification=byte-integrity-only`、`restore_verified=false`，不代表 AEAD、账本不变量或备份来源真实性通过。攻击者能同时替换文件和哈希，SHA256 不是签名。

在线保留期清理现在仅删除经上述完整性检查的过期包。异常包保留并发出需要人工核查的日志；运营必须监控该告警及磁盘容量，不能把保留异常包当作已完成修复。

离机灾备仍需运营提供受控外部存储、最小权限身份、传输/静态加密与保留策略，并完成真实上传后从另一台机器取回。`KIRO_MASTER_KEK` 必须有独立保管和取回流程，不放入此备份包或公开 CI artifact。取回后应在空白隔离 Linux 环境用真实 verifier 和正确密钥执行恢复演练，记录备份时间、故障时间、恢复时间、RPO/RTO、账本核对及责任人。此变更不配置外部存储或自动上传，也不构成离机恢复证据。
