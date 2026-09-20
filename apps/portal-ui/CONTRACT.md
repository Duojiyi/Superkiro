# 官网部署与 API 契约

## 静态入口

唯一运行时文件：`index.html`。CSS、JS、概念图全部内嵌，无框架、字体请求或第三方脚本。`CONTRACT.md` 和测试截图无需公开发布。

Caddy 应将 `/`、`/device`、`/device/*`、`/docs`、`/docs/*`、`/portal`、`/portal/` 映射到此文件。`/portal` 为首页别名。`/docs/<章节>` 和 `/docs#<章节>` 均支持 start、models、device、restore、connection、downloads。未知文档章节回到快速开始。`/device/*` 显示换机页。

`/admin/` 保持现有后台与管理员鉴权；`/api/*` 转发后端；`/downloads/*` 提供真实公开发布文件，不回退 HTML。生产站点只经 HTTPS 提供服务。卡密及请求体不得进入反代/应用访问日志。API 响应应设置 `Cache-Control: no-store`，不得被 CDN 缓存。

当前 Rust 只注册 GET `/portal`，本轮不修改 Rust；上述静态路径由维护者配置 Caddy。

## GET /downloads/releases.json

返回 HTTP 200、`Content-Type: application/json`。结构为 `{ "releases": [] }`；数组中每个当前平台架构至多一条，未发布可省略，不要求三平台同时发布。建议对清单设置 no-cache，先上传真实安装包，再原子更新清单。

每条记录的必需字段：

| 字段 | 类型及语义 |
| --- | --- |
| platform | `windows` 或 `macos` |
| arch | Windows 为 `x64`；macOS 为 `arm64` 或 `x64` |
| version | 非空字符串，真实版本号，不需要前缀 v |
| url | 同源 `/downloads/...` 路径或公开 HTTPS 发布存储 URL；不允许凭据、HTTP 外链、GitHub 仓库/artifact URL |
| sha256 | 安装包真实的 64 位十六进制 SHA-256 |
| size | 安装包实际字节数，正整数 |
| systemRequirements | 真实系统要求，非空字符串 |
| signature | 安装包签名状态：`signed`、`unsigned` 或 `unknown` |

`signature` 不表示清单自身已签名或 Mac 已公证。当前产品边界仍明确提示 Mac 未签名/公证及 IDE 实机验收未完成；未来状态变化须同步更新文案，不通过一个 signed 字段自动解除测试版提示。

页面不合成下载地址、不提供默认版本。清单不可用、格式错误或条目校验失败时禁用相应下载；重复平台架构也禁用。校验值提供给用户核对，浏览器没有实现安装包下载后的自动验签，也没有伪称实现签名清单验证。需要签名清单时，另行约定算法、可信公钥与发布流程。

## 已对接的后端

依据 `crates/gateway/src/facade/portal.rs` 及 `crates/billing/src/engine.rs`；均只读未修改。

1. 验证：POST `/api/v1/portal/query`，JSON `{ "card": "用户输入" }`。只读查询，不调用 activate、客户端认证/绑定或任何生成设备标识的接口。
2. 查询成功：`success: true`，使用 `virtualPlanName`、`status`、`remainingPoints`、`validUntil`（Unix 秒，可为 null）、`isExpired`、`boundDevices`（真实设备标识数组）。仅渲染脱敏设备尾号；真实值只保留于当前页面内存，用数组索引作为 select value。不使用 groupName 暴露内部供应商信息。
3. 用户确认后 POST `/api/v1/portal/challenge`，JSON `{ "action": "unbind" }`，读取 `challengeToken`。现有服务端 challenge 有效期 120 秒，与 IP/action 绑定，一次性消费。
4. 紧接着 POST `/api/v1/portal/unbind`，JSON `{ "card": "用户输入", "device": "查询返回的真实设备标识", "challenge_token": "本次challengeToken" }`。确认 `success: true`、`remainingDevices` 数组且原设备不在其中，才展示成功。不自动重试解绑；超时/失败后清除账户状态，要求重新查询确认。
5. 429 读取 Retry-After 秒数；403 显示凭证失效/拒绝；400 显示脱敏失败原因；404/405/501 显示服务未开放。后台原始错误不直接展示，避免泄露卡密、设备标识或内部信息。

卡密只经 HTTPS POST 请求体提交（本机 localhost 测试例外），不写 URL、Storage、cookie、日志、分析事件。离页/重新选择卡密/解绑结束清空页面状态。卡密输入 password，不提供网页绑定能力。

## 验证与计时边界

客户端 `run_desktop.py` 的 `verify_card` 与网页验证均只查询 portal API，不激活、不绑定，也不启动未激活卡密的有效期计时。客户端需由用户手动确认“启用连接”后才请求激活与绑定；未激活卡密从服务端成功激活时开始计时，不等待 IDE 连接完成或首条消息。已激活卡密再次验证或连接不会重新计时。

## 换机策略与展示边界

服务端 `unbind_device` 已复用卡密的 `max_rebinds` 与 `rebind_cooldown_secs` 检查次数和冷却，并在同一持久化事务内移除设备、消耗一次换机次数、更新 `last_rebind_at` 及递增 `token_version`。积分与有效期保持不变。

解绑后的空槽允许新客户端立即绑定，完成本次换机，不重复消耗次数，也不要求等待本次解绑开启的冷却结束。首次绑定同样不消耗换机次数；后续再次换机仍受服务端策略限制。

网页说明统一为“换机受卡密的次数与冷却限制，以服务端结果为准”。不硬编码次数、冷却或倒计时，不自行用 `rebindCount`/`maxRebinds` 推算“可解绑”状态，不在前端阻止新客户端立即绑定。卡密验证仍为只读查询，不绑定网页设备。

当前 query API 未提供可解绑权限、冷却截止时间、设备最近使用时间或设备名称；页面不伪造这些信息。这是展示边界，不代表服务端未执行限制。若以后需要展示精确可操作时间，可另行扩展查询契约，不作为当前换机流程的前置条件。

上述策略已核对本地后端补丁；不代表官网或后端已经部署到生产。

## 本地验证

从仓库根执行：`python -B test_superkiro_portal_ui.py`。使用已安装的 Playwright 与 Chromium，临时本机 HTTP 服务模拟 Caddy 页面映射，API/清单均为测试桩，不接触生产卡密。截图只写 `apps/portal-ui/test-artifacts/`。测试不证明真实包存在、生产路由已配置或服务端策略已经实现。
