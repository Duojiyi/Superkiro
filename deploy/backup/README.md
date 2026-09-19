# Kiro BYOK Gateway 快照备份与恢复指南 (Spec §7, T07)

当前网关使用**单机持久化内存账本 + `billing_state.json` 快照与代际锚点（Generation & Anchor）架构**。本文档详细说明生产环境备份与灾难恢复（Disaster Recovery）的严格规程与验证机制。

## 一、备份体系 (Backup System)

### 1. 一致性与停写保护
- Compose 生产部署不发布 gateway 容器端口；脚本同时检查 `HEALTH_URL` 和 `docker compose ps`，不能用“主机端口不可达”判断已停机。
- 恢复若无已构建的 gateway verifier，会调用 `cargo run`；无 Rust verifier 时 fail-closed，不接受 Python 结构校验。
- **在线备份**：当网关正在运行时，必须提供 `ADMIN_KEY`。脚本会自动请求 `POST /api/v1/admin/snapshot/sync` 触发网关内存账本同步刷盘，避免读到中间写状态；
- **冷备份**：网关离线时执行，直接读取磁盘中已原子提交的快照与锚点；
- **防护机制**：若网关在线且未提供 `ADMIN_KEY`，脚本默认拒绝执行（可通过 `--force` 强制覆盖，但可能存在并发脏读风险）。

### 2. 备份命令
```bash
# 1. 在线一致性备份（推荐）
ADMIN_KEY="your-admin-master-key" ./deploy/backup/backup.sh

# 2. 离线冷备份
./deploy/backup/backup.sh

# 3. 自定义路径与保留周期
DATA_DIR=/srv/kiro/data BACKUP_DIR=/srv/kiro/backups RETENTION_DAYS=14 \
  ADMIN_KEY="your-admin-key" ./deploy/backup/backup.sh
```

### 3. 备份生成四件套与原子提交
每个备份在 `${BACKUP_DIR}` 下生成唯一的代际标识（`YYYYMMDD_HHMMSS_<random>_<pid>`），彻底杜绝同秒任务文件名冲突：
1. `billing_state_<gen>.json`：快照主数据（支持 AEAD 加密密文或 JSON）；
2. `billing_state_<gen>.json.anchor`：快照代际锚点，记录版本号、序列号与 SHA-256 校验和；
3. `billing_state_<gen>.json.sha256`：快照主数据的独立 SHA-256 校验文件；
4. `billing_state_<gen>.manifest.json`：**原子提交点（Commit Point）**。包含代际元数据、各文件校验和及 `status: completed`。未写完前不发布成功标记；任何缺失 manifest 的备份均被视为不完整备份。

### 4. 集合式保留期修剪
备份清理严格以 `manifest.json` 为单位，统一删除整套代际文件（`.json`, `.anchor`, `.sha256`, `.manifest.json`），严禁单独删除单文件导致出现残缺孤儿半套备份。

---

## 二、恢复体系 (Disaster Recovery System)

### 1. 运行态防护 (Live Overwrite Prevention)
恢复脚本会前置探测网关运行状态。若网关处于运行中，脚本将**严格拒绝执行恢复**，防止运行中网关的内存状态在下一次快照定时刷盘时反向覆盖掉新恢复的数据。恢复前必须先停止网关服务。

### 2. 临时沙箱全量引擎验证 (Engine Pre-Verification)
在触碰 live 数据前，脚本自动创建临时隔离沙箱并调用实际的 Rust 账本引擎（`BillingEngine::verify_snapshot_integrity`）执行全面核对：
- **AEAD 解密有效性**：验证 `KIRO_MASTER_KEK` 密钥正确性（密码学认证 Tag 校验）；
- **代际与版本校验**：验证快照格式版本、代际序列号（Sequence）；
- **校验和匹配**：验证快照内容与 `.anchor` 锚点及 SHA-256 sidecar 严格一致；
- **全量卡密与账本平账核对**：遍历所有卡，将卡内余额与不可篡改的流水账本（Usage / Adjustment / Topup）逐一重新计算，断言 `expected_credit_total == actual_credit_total` 且 `credit_used == ledger_usage_sum`。

**任何一项不合格，立即在沙箱阶段阻断报错，绝不触碰生产目录！**

### 3. 完整回滚代际与替换自愈
- 在执行 live 替换前，脚本自动将现有的 `billing_state.json`、`.anchor` 以及历史 `gen_*` 代际打包至 `.rollback_<timestamp>_<pid>/` 目录并写入回滚清单；
- 采用同文件系统原子替换（`mv -f`）与 `sync` 刷盘；
- 若替换过程中发生任何系统级异常，错误捕获陷阱（Trap）会自动从 `.rollback_*` 目录回滚恢复，确保系统零丢失、零残损。

### 4. 恢复命令
```bash
# 传入备份文件或对应 manifest 路径执行恢复
./deploy/backup/restore.sh ./backups/billing_state_20260915_120000_1234_5678.manifest.json

# 或直接传入快照主文件
./deploy/backup/restore.sh ./backups/billing_state_20260915_120000_1234_5678.json

# 反向代理场景可显式指定健康地址（仍会检查 Compose 容器状态）
HEALTH_URL=https://kiro.yourdomain.com/healthz ./deploy/backup/restore.sh ./backups/billing_state_20260915_120000_1234_5678.json
```

---

## 三、安全与运维规约
1. **密钥隔离**：`KIRO_MASTER_KEK` 严禁写入备份包或日志输出中；
2. **多介质备份**：建议将 `${BACKUP_DIR}` 挂载至独立存储卷，或由定时任务向 S3/对象存储同步；
3. **定期演练**：生产环境建议每月在测试环境运行 `restore.sh` 配合自动化校验，确保冷备数据随时可用。

