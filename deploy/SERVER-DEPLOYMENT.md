# IP HTTPS 部署与验收

本次服务器：160.202.47.98。代码目录 /opt/kiro-byok/current，数据目录 /opt/kiro-byok/data。

## 安全与访问

- IP 已由 160.202.47.231 更换为 160.202.47.98，SSH 主机指纹与原记录一致。新 IP 的本地直连和 HTTP 代理 http://127.0.0.1:7897 HTTPS 检查均通过；现有 SSH 验收脚本仍使用 CONNECT 隧道。
- 首次观察并固定的 SSH ED25519 指纹：SHA256:sTYVluUx3J9nai3Cj67JvjQ5+DRuqLjjiWXT1s+wyiI。它并非经供应商控制台独立核验。
- 当前使用私有 CA 的 IP HTTPS。公开证书在 deploy/server-ca.pem；Python 测试使用 verify 参数，浏览器测试固定经 SSH 获取的服务器叶证书 SPKI 公钥，不全局关闭证书校验。
- 浏览器默认不信任该 CA；正式对外使用应绑定域名并使用公有 CA 证书。不要忽略所有 TLS 错误。
- 密钥配置仅在服务器 /etc/kiro-byok/gateway.env（0600）。后台双层认证信息在 /etc/kiro-byok/admin-access.json（0600）。不要提交或粘贴其内容到日志。
- 服务使用 Anthropic 协议，https://kimera.cc，claude-sonnet-4-6；API key 不在仓库中。
- 19820 仅在容器网络可见；公开端口为 80/443，80 重定向至 HTTPS。

## 运行与验证

服务器：

~~~sh
cd /opt/kiro-byok/current/deploy
docker compose -f docker-compose.ip.yml ps
docker compose -f docker-compose.ip.yml logs --tail=100 gateway
systemctl status kiro-backup.timer
~~~

本地：

~~~powershell
python test_deployed_server.py
# 交互输入 SSH 密码；自动化可通过 --stdin-config 从 stdin 提供 JSON。
~~~

该测试会创建测试卡并最终禁用，发送少量真实付费请求，并重启网关验证恢复。
只应在维护窗口运行。结果输出 deployment-e2e-results.json，不保存卡密或会话令牌。
浏览器测试 test_deployed_browser.py 通过 stdin 接收相同密码 JSON，凭据仅驻留内存。

## 备份与恢复

- 每日 UTC 03:30（北京时间 11:30）执行加密账本备份，保留 7 天，由 systemd timer 管理。
- 手动备份：systemctl start kiro-backup.service。
- 密钥不包含在账本备份内；恢复必须保留对应的 KIRO_MASTER_KEK，单独安全托管。
- 同机备份不能抵御整台主机或磁盘丢失；上线前应配置独立加密异地备份。
- 回滚应用时保留 /opt/kiro-byok/data 和 /etc/kiro-byok，切回经过验证的旧镜像；不得用空数据目录覆盖现有账本。
- 本次是全新主机首次部署，没有旧应用版本可回滚。

## 功能边界

后台分组动态创建、在线定价发布、TOTP 2FA、部署级 RLS 并未实现；界面禁用或标记说明不代表功能已完成。
MCP 协议握手不等于外部搜索后端已配置。默认代码补全明确限流，没有接入专用补全模型。
桌面桥接单元测试不等于已在真实 Kiro IDE 内完成全部交互验收。

## 本次验收结果

- Rust 工作区测试 336 项通过，Clippy 严格检查通过，前端构建通过。
- 桌面桥接回归 6 项通过，本地模拟上游集成检查通过。
- 本地经代理访问生产 HTTPS 的 19 项端到端检查通过，包含真实模型文本、工具调用、扣费幂等和重启恢复。
- 浏览器检查覆盖 9 个后台页面和用户门户，无页面 JavaScript 异常或 HTTP 5xx。
- 备份定时器已启用；备份解密、完整性和账本平衡校验通过，尚未在独立运行实例中进行完整还原演练。
- 测试卡已禁用；生产凭据没有写入本地测试结果。

## IP 变更复查

新 IP 的直连与代理 HTTPS 健康检查、门户访问、后台未认证拦截共 6 项通过，使用原私有 CA 校验新证书。仅重建 Caddy 以加载新地址，未重启网关或修改上游凭据。备份手动执行成功。本次未重复执行付费模型与重启恢复测试，前述 19 项结果属于首次部署验收。
