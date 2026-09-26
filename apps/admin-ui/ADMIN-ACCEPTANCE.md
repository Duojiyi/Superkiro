# 管理员日常流程验收（本地，未部署）

## 已接入

- 登录：用户名 `admin` + 密码；服务端 `ADMIN_BROWSER_LOGIN=true`，`ADMIN_ORIGIN` 必须是精确 HTTPS origin，`ADMIN_PASSWORD_HASH` 为 bcrypt。`ADMIN_KEY` 仅留服务端，不输入浏览器。
- Cookie：`__Host-admin_session`、Secure、HttpOnly、SameSite=Strict；30 分钟无操作失效，最长 8 小时（自动刷新请求带 `x-admin-background: 1`，不算操作）；刷新恢复会话；写请求携带 `GET /api/v1/admin/session` 返回的 CSRF。登录全局每分钟 10 次预算。
- 认证门禁：checking 时仅检查会话；未认证只渲染独立登录页，无取消入口，不请求后台数据。401 或退出立即卸载后台、清除会话令牌并中止在途请求；重登录不保留筛选或编辑草稿。服务端撤销失败仍保持本地退出，并明确提示撤销未确认。
- 卡密：分页读取、搜索筛选、四档批量生成、结果表格、逐卡/整批复制与 CSV、冻结/解冻/封禁、带原因调账。按需 POST `/cards/reveal`，关闭清除明文；`codeRecoverable=false` 禁用并说明旧卡不可恢复。
- 供应商：渠道导入、启停、Key 权限/权重/状态保存、候选模型发现（不会自动发布用户目录）。缺少必填项的操作禁用。
- 商业配置：分组、模型映射、固定积分新价格版本、高级 JSON；提交变更原因与 revision；历史价格只读。
- 追踪：查询、筛选、详情、请求 ID 复制、二次确认清理旧追踪；财务账本 CSV/JSON 导出；公告预览、二次确认发布。

## 只读与未接入

- 概览为最近追踪样本，不是全天聚合；日期范围选择禁用。
- 配置审计及历史价格只读，不代表完整安全审计覆盖。
- 真实到账收入、可核验采购成本和毛利未完成对接，界面明确待接入，不伪造数值。
- 独立连通性测速、TOTP、Web KEK 轮换未接入；KEK 只提供运维说明。
- 公告当前仅支持读取和发布，不提供编辑、撤回；旧卡无恢复密文时不能恢复。
- 供应商候选模型发现不证明实际可调用，上游连通性及生产权限需要独立验证。

## 验证边界

- `npm run build`：TypeScript 与 Vite 构建。
- `node tests/auth-session.cjs`：匿名请求门禁、无效会话、迟到 JSON/导出响应、跨会话 401 隔离及退出失败。
- `node tests/auth-gate.cjs`：构建产物的三态门禁、认证前零后台请求、无法取消绕过、401 卸载草稿、立即退出和单一错误提示。
- `node tests/contracts.cjs`：Cookie/CSRF、401、退出失败、501 张分页、reveal、制卡与日常写操作请求契约。
- `node tests/visual-authenticated.cjs`：隔离 fixture API；九页面桌面/移动端、四档制卡、搜索、冻结解冻、reveal/隐藏、旧卡禁用、下载、追踪详情、过期重登录重置筛选和退出清理。
- UI fixture 测试不等于真实数据库写入、供应商上游调用或生产环境验收。新增 `admin_browser_login_test` 走生产路由 Cookie 登录、GET session、真实 reveal、logout 和缺失配置 fail-closed；Rust 测试结果以本次任务最终报告为准；不修改 billing/卡密后端以绕过编译问题。
