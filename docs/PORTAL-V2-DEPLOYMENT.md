# 门户 V2 生产发布与手动验收

2026-09-18，服务器 `160.202.47.98`。

## 已上线

- 用户门户：https://160.202.47.98/portal
- 网关地址：https://160.202.47.98
- 新页面源文件：`apps/portal-ui/index.html`，无运行时 CDN、无新增前端依赖。
- Caddy 静态发布 `/portal` 与 `/portal/`，其余接口继续由现有网关处理。
- 本轮只重建 Caddy 加载只读门户挂载，未重启网关、未覆盖账本、未修改上游密钥。
- 上游现有配置已与用户提供值核对一致：Anthropic 协议、`https://kimera.cc`、`claude-sonnet-4-6`。密钥只在服务器受保护配置及验证进程内存中使用。
- 本地 Rust 的门户 HTML 改为包含同一份静态文件，Dockerfile 同步拷贝该文件，保证后续构建的一致性。本次线上网关镜像仍为原有已运行镜像，门户由 Caddy 提供。

## 回滚

备份目录：`/opt/kiro-byok/backups/portal-20260918-201752`。
保存了原 `Caddyfile.ip` 和 `docker-compose.ip.yml`。恢复这两份文件到 `/opt/kiro-byok/current/deploy/`，执行：

```sh
cd /opt/kiro-byok/current/deploy
docker compose -f docker-compose.ip.yml up -d --no-deps --pull never caddy
```

不要覆盖 `/opt/kiro-byok/data` 或 `/etc/kiro-byok`。移除门户路由后会回到网关内原有门户。

## 验证证据

- `deployment-portal-v2-results.json`：本地直连/代理均 HTTP 200；线上 HTML SHA256 与本地文件一致；上游真实调用成功；网关真实 EventStream、用量扣费、请求幂等与解绑通过。
- `deployment-portal-v2-browser-results.json`：线上真实查询、激活、取消保留绑定、解绑、无效充值券拒绝、手机四页无溢出、帮助弹窗通过，没有 JavaScript 页面异常。
- `test_portal_ui.py`：模拟接口覆盖查询/激活/换绑/充值成功、错误保留输入、限流、返回内容安全渲染、移动布局。
- `cargo test --locked -p gateway --test portal_test`：6 项接口集成测试通过，含充值兑换成功。
- `cargo test --locked -p patch-engine --lib`：9 项通过，含私有 CA 读取与无效内容拒绝。
- 桌面桥接、桌面 UI 与门户 UI 合计 10 项 Python 测试通过。
- `test_desktop_native.py`：真实 WebView2 窗口尺寸和设置页往返通过。
- 临时自动化卡已禁用。给用户的卡是另一张全新卡，不使用它执行模型探针或浏览器激活。

## 客户端准备

- Debug 已重新编译，并与 Release 共用客户 UI。
- 启动入口预填新服务器网关，可由 `KIRO_GATEWAY_URL` 覆盖。
- Rust 客户端通过 `KIRO_GATEWAY_CA_CERT` 加载应用级可信 CA；当前启动器使用 `deploy/server-ca.pem`。未修改系统证书库，未关闭 TLS 验证。
- Kiro 使用 `启动Kiro验收.bat` 启动，为该进程设置网关变量和 `NODE_EXTRA_CA_CERTS`。必须先完全退出已有 Kiro，使环境生效。
- 手动测试卡详情保存在 `.acceptance/private/manual-card.json`，目录已限制为当前用户与 SYSTEM；不在公开报告中记录卡密。

## 明确边界

- 当前是私有 CA 的 IP HTTPS，不是公有可信证书。普通浏览器可能显示证书不受信任，正式对外发布仍应配置可信域名/公有证书。
- 线上充值本轮验证了错误分支；充值成功由后端集成测试和模拟浏览器覆盖，没有在生产兑换真实充值券。
- 未代用户激活测试卡或修改当前 Kiro 配置；真实 IDE 交互由用户输入卡密后验收。
- 外部搜索、专用补全、动态分组、在线定价、TOTP 和数据库 RLS 不因本轮门户发布而变成已实现。


## 后续桌面一键接管修订（本地代码）

- 内置默认网关；激活按钮明确确认关闭 IDE，桥接仅转发布尔确认值。
- 接管前验证安装、补丁兼容性和 CA；Windows 请求关闭主窗口，等待最多 30 秒，取消/超时不会强杀或修改配置。
- 成功接管后自动携带网关变量与 NODE_EXTRA_CA_CERTS 启动，不再要求手工运行 Kiro 验收脚本；不修改系统根证书库。
- 每次显式激活均验证输入卡密，不再刷新旧卡会话。无效卡不会覆盖原 token。
- 自动启动仅代表发出了进程启动请求，不代表模型列表、工具调用或扣费验收通过。
- 激活失败后不会自动启动；原有快照保留用于恢复。关闭后认证失败时，IDE 保持关闭，界面显示错误。
- 正常快捷方式直接重开 Kiro 是否继承全部环境仍未保证；目前应通过接管客户端启动。
- 此修订没有更改线上模型映射、定价或账本；真实 IDE 验收仍待完成。
