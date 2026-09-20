# Kiro BYOK 单机一体化生产部署手册 (Single-Server Deployment)

本指南针对**单台服务器**场景设计，遵循 YAGNI 极简与高可用原则。通过 **Docker Compose + Caddy 自动申请免费 HTTPS + 内存/本地持久化网关**，实现零运维、开箱即用的高并发大模型接管服务。

---

## 1. 架构总览

```
                     Internet (用户请求 / 发卡平台 / Kiro IDE 客户端)
                                     │
                                     ▼
                       [Caddy 反向代理 (端口 80 / 443)]
                         ├── 自动申请与续签 Let's Encrypt TLS 证书
                         ├── Event-Stream / SSE 流式响应不缓冲保活
                         └── HSTS / 安全防护响应头
                                     │ (内网桥接 kiro-net)
                                     ▼
                      [Kiro Gateway 容器 (端口 19820)]
                         ├── Kiro IDE 冒充路由 (虚拟模型/用量/订阅)
                         ├── 双向流式二进制转译 (OpenAI/Anthropic <-> Kiro Wire)
                         ├── Web 自助激活与充值门户 (/portal)
                         ├── 自动化发卡平台对接口 (/api/v1/cards/*)
                         ├── 客户端版本协商与信标心跳 (/client/*)
                         └── 本地状态持久化挂载卷 (/app/data)
```

---

## 2. 准备工作

### 2.1 服务器环境要求
- **操作系统**: Ubuntu 22.04 LTS / Debian 12 / CentOS 9 Stream / Alpine Linux 等
- **必备组件**: 已安装 Docker 与 Docker Compose v2+ (`docker compose`)
- **网络与防火墙**: 开放公网 `80` 与 `443` 端口（若直接暴露网关测试，可开放 `19820`）
- **域名解析**: 将您的域名（如 `kiro.example.com`）解析至该单台服务器公网 IP

---

## 3. 一键快速部署

### 步骤 1：进入部署目录并配置环境变量
```bash
cd deploy
cp .env.example .env
vim .env
```

`.env` 核心配置项说明：
```dotenv
# 1. 域名 (Caddy 会自动为其向 Let's Encrypt 申请 SSL 证书并配置 HTTPS)
DOMAIN=kiro.yourdomain.com

# 2. 网关内部鉴权密钥 (至少 32 字符随机字符串)
AUTH_SECRET=a_very_secure_random_string_32_characters_long!
# 发卡平台对接密钥 (保护 /api/v1/cards/* 接口)
CARD_PLATFORM_KEY=secret-card-platform-token-here

# 3. 上游大模型服务商 API (支持 openai / anthropic 兼容接口)
PROVIDER_TYPE=openai
UPSTREAM_BASE_URL=https://api.deepseek.com
UPSTREAM_API_KEY=sk-xxxxxxxxxxxxxxxxxxxxxxxx
UPSTREAM_MODEL=deepseek-chat
# 非标准兼容服务可填写完整聊天端点；网关会原样使用，不再重复追加路径。
# 例如：UPSTREAM_BASE_URL=https://provider.example/api/chat/completions

# 4. 生产不预置公开示例卡密，请登录管理后台生成卡密。
# 可选 DEV_CARD_CODE 必须是独立生成的强随机秘密。

# 5. 品牌与客户端展示名称
BRAND_NAME=Superkiro
CREDIT_LABEL=积分
```

### 步骤 2：启动容器集群
```bash
docker compose up -d --build
```
> **说明**：首次启动会自动编译 Rust release 二进制镜像并拉取 Caddy Alpine 镜像，耗时约 2-3 分钟。若在 Linux 上运行且宿主机 `./data` 目录为新建目录，确保其对容器内用户有写权限。容器使用 UID/GID 1000；启动前在仓库根目录执行 `sudo install -d -m 0700 -o 1000 -g 1000 data`。已有数据先停机备份，再将所有者设为 `1000:1000`，目录权限 `0700`、普通文件权限 `0600`，禁止使用世界可写权限。

---

## 4. 状态检查与验证

### 4.1 查看容器运行状态
```bash
docker compose ps
```
正常应显示 `kiro-gateway` 与 `kiro-caddy` 状态均为 `Up` (healthy)。

### 4.2 查看运行日志
```bash
# 查看网关实时日志
docker compose logs -f gateway

# 查看 Caddy 反代与证书申请日志
docker compose logs -f caddy
```

### 4.3 验证端点连通性
```bash
# 验证健康检查接口 (HTTP 200)
curl -i https://kiro.yourdomain.com/healthz

# 验证客户端协商接口 (HTTP 200)
curl -X POST https://kiro.yourdomain.com/client/negotiate \
  -H "Content-Type: application/json" \
  -d '{}'

# 验证发卡平台拉码接口 (带授权密钥)
curl -X POST https://kiro.yourdomain.com/api/v1/cards/pull \
  -H "Content-Type: application/json" \
  -H "X-Card-Platform-Key: secret-card-platform-token-here" \
  -d '{"order_id": "order-001", "count": 1}'
```

### 4.4 访问 Web 门户
打开浏览器访问：`https://kiro.yourdomain.com/portal`
- 即可看到现代暗色磨砂玻璃风格的自助激活、余额查询与换绑门户；
- 换机页面使用管理员分发的真实卡密查询和解绑，不创建公开示例卡。

---

## 5. 数据备份与恢复

所有业务数据、卡密账本与持久化快照均存储在仓库根目录的 `./data` 目录中（Compose 将其挂载为容器内 `/app/data`）。

### 5.1 在线一致性备份
```bash
sudo ADMIN_ORIGIN=https://kiro.example.com python3 deploy/backup/server-backup.py
```
默认部署根目录 `/opt/kiro-byok`；凭据文件 `/etc/kiro-byok/admin-access.json` 必须由 root 或执行用户拥有、权限 0600，内容为管理员 username/password，启用 TOTP 时还需 totpSecret。不要将凭据写入命令行或版本控制。脚本使用 HTTPS、Cookie 与 CSRF，备份已同步的不可变代际。

### 5.2 冷备份与恢复
先停止网关，再执行 `deploy/backup/backup.sh`。在线时此脚本会拒绝运行，不提供绕过。恢复同样要求停止网关，并通过真实 Rust verifier 检查账本；在线备份目录内的主快照可作为 `restore.sh` 输入。

完整权限、定时任务与恢复演练说明见 [备份指南](backup/README.md)。

---

## 6. 服务升级与日常维护

### 6.1 代码拉取与平滑更新
```bash
git pull
cd deploy
docker compose build gateway
docker compose up -d --no-deps gateway
```

### 6.2 停止与重启
```bash
docker compose restart
# 或完全停机
docker compose down
```
