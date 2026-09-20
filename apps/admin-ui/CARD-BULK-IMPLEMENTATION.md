# 管理端卡密批量操作实现与定向检查

本轮为实现者自检，不是独立或双盲审计。未部署、未连接生产服务、未修改生产数据。

## 部署边界

仅前端静态变更；复用当前网关 GET /api/v1/admin/cards、POST /api/v1/admin/cards/status 和 POST /api/v1/admin/cards/reveal。没有新增 API、网关代码或计费语义变更。上线前应由部署方确认目标网关已有这些接口以及管理员 Cookie/CSRF、reveal no-store 支持；本轮未核验生产版本。

## 行为

- 本地每页 50 条，仅显式选择当前页；可单选、当前页全选、清空。
- 批量冻结/解冻/封禁逐项调用既有状态接口，危险动作确认实际数量，进行中锁定选择和操作；请求失败不自动重试。
- 冻结仅用于 active，解冻仅用于 frozen，封禁跳过已 banned，沿用现有单卡入口规则。不适用项列为未执行。
- 逐项展示成功、失败或结果未确认。完成后自动刷新，成功项取消选择，失败及未执行项仅在当前页保留；筛选、翻页、离开卡密页及手动刷新清空选择。提交时再次取当前页与已选 ID 的交集。
- 已选导出使用管理员 reveal，输出 UTF-8 TXT，每行一条成功读取的卡密。旧卡不可恢复与读取失败分别显示；没有成功项不下载。秘密仅短暂存在函数局部变量和下载 Blob，不存 React 结果、localStorage 或 sessionStorage；下载后释放 Blob URL。会话失效停止后续请求，不下载之前已读取的部分秘密。
- 下载仅表示已发起浏览器下载，不声称用户已经保存成功。

## 验证

- npm test：通过（契约、会话、到期相关测试）。
- npm run build：通过。
- npm run test:card-bulk：本地隔离 fixture，通过选择范围、取消确认、防重复、部分失败选择保留、三种状态动作、状态不适用跳过、筛选不携带旧失败项、导出内容、秘密不落存储及会话到期测试。
- npm run test:usability：通过（公开登录与九页面窄屏、弹窗、剪贴板和公告流程）。
- cargo test -p gateway --test admin_api_test --test admin_card_recovery_test --test admin_browser_login_test：12 项通过。
- npm run test:browser：通过。已修正 auth-gate.cjs 的过时假设：默认无 2FA 时不显示验证码并完成密码登录；独立匿名上下文通过登录 POST 返回 totpRequired 挑战，验证空值、短码、非数字不发请求，六位数字携带 totpCode 提交，拒绝后清空敏感输入且不进入工作台。业务逻辑未修改，截图与测试结果写入系统临时目录。

## 其他发现与保留边界

- 现有 getCards 仍分页拉取全量元数据，再在前端分页/筛选；本轮限制的是操作范围，不是元数据读取范围。大规模数据优化应另行设计服务端筛选与稳定分页。
- 底层 freeze_card 无原状态约束，unfreeze_card 对非 frozen 状态不转换，而 facade 成功响应的 newStatus 固定为 active。本轮前端遵循单卡可用状态做跳过；并发管理员修改状态仍存在检查与提交间的竞争窗口，未擅自改变状态/计费语义。
- reveal 既有审计记录 actor 为通用 admin，状态冻结/封禁原因进入卡备注，解冻原因未传递给 billing；这不等同于带独立操作者身份的统一持久审计账。页面逐项结果也只是本次会话结果，不冒充持久审计。
- 批量不是事务，允许部分成功；网络失败可能实际已经提交，应先核对刷新状态再人工重试。

## 补测环境

- 使用既有 dist/assets/index-Df6i_H9H.js，未重建或部署；test:browser 与 test:card-bulk 再次通过。
- PowerShell：`$env:PLAYWRIGHT_MODULE='C:/Users/Administrator/.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules/playwright'`。
- `CHROME_PATH` 未设置；Playwright 默认 Chromium 路径为 `C:/Users/Administrator/AppData/Local/ms-playwright/chromium-1234/chrome-win64/chrome.exe`。如需明确指定，可将该路径赋给 `CHROME_PATH`。
- 测试仅使用 localhost fixture，不代表生产验收；生产部署与验收由父代理执行。
