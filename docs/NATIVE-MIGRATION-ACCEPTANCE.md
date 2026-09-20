# Superkiro 原生迁移验收记录（2026-09-19）

状态：2026-09-19 本轮已部署服务端 20260919T070809Z，并发布 Windows 原生测试包 native.2。下面早期记录保留为历史；以文末最新发布验收为准。

## 已实现的边界

- 客户端为 Rust/Tauri 2 + React/TypeScript。Python 仅用于构建脚本，不是新客户端运行时；Windows 仍依赖系统 WebView2。
- 卡密查询与启用分离；本地凭据不可读时仍保留诊断/恢复入口。恢复门禁同时检查配置快照与会话回滚文件。
- 余额刷新失败不再显示旧值为当前值；用量日窗明确 UTC。内存统计只计 Kiro 所属进程，采样失败不制造释放量。
- 管理端表单登录使用 HttpOnly/Secure/SameSite Cookie 和 CSRF，生产不再接受浏览器输入管理员密钥。
- 新卡恢复密文使用既有 KEK；查看需鉴权且 no-store。旧 hash-only 卡不能恢复明文。卡密编号冲突拒绝整批写入，不覆盖现有卡。
- 管理员查看卡密的结果写入服务日志；这不是持久、可查询的完整安全审计系统。

## 已取得的证据

- `.acceptance/desktop-core-full-final.log`：此前 patch-engine 全套 68 项通过；最后恢复门禁修改需单独复验。
- `.acceptance/card-collision-regression.log`：billing 卡密恢复/碰撞 4 项、生产 Cookie 路由 1 项、管理卡密恢复 2 项通过。
- `.acceptance/admin-final-integration.log`：管理 API、Cookie、卡密恢复及卡平台 17 项通过（先于最后日志/碰撞修改）。
- `python -m unittest deploy.test_browser_auth_contract`：3 项离线契约检查通过，不操作生产。
- 桌面 React 与管理后台的 fixture/mock 浏览器结果仅证明界面和请求契约，不证明真实 Kiro、真实上游或线上权限。

## 早期候选待办（历史状态，见文末更新）

1. 最终原生 EXE 的窗口、托盘、关闭/恢复以及真实 Kiro 请求/扣费全链路尚需验收；用户验收卡不得用于自动化消耗。
2. Windows 原生圆角、缩放、最终资产嵌入及设计一致性需实际窗口复核，不能凭 CSS 截图判定 1:1。
3. macOS CI 配置已有 arm64/x64 构建，不代表实际构建成功；Developer ID 签名、公证和 Mac 原生交互仍未验证。
4. 管理认证上线需配套 Caddy/env 迁移、可恢复备份与配置回滚。详见 `deploy/ADMIN-LOGIN-MIGRATION.md`。旧自动备份入口仍需迁移或明确暂停。
5. TOTP、部署级 RLS、完整业务财务核算及持久安全审计不能标记已完成。管理 UI 未接入项必须明示，不得用示例数据冒充真实结果。
6. 根目录历史桌面启动 bat 仍指向旧 Python 客户端，不能作为新原生版验收入口。

生产状态以独立线上验收为准。本轮未上传新包、未替换公开下载、未执行认证配置迁移。

## 最后前端复核

- 桌面 React 最终版本：22/22 单元测试、17 个 mock 浏览器场景通过；JS `index-BRzntDz2.js`、CSS `index-CEq_GXXE.css`。
- 登录正常态与错误态均用 480×620 验证，无横纵溢出，恢复/重试/获取卡密入口可见。
- 全局主按钮统一银灰竖向金属渐变，登录光栅修正为中央单峰。透明根与 Windows transparent 宿主配合，浏览器截图四角透明；真实原生圆角仍需验证。
- `desktop-host-test.log`：最终恢复/UTC相关修改后 11 项宿主测试通过。
- 已成功构建首个约 12 MB 的本地原生 EXE，但首次原生启动工具超时、未取得可见窗口；不可标记原生启动验收通过。最终增量包结果需另记。

## 本机真实回归补充（2026-09-19）

- 用户重新允许自动化后，真实 Rust/Tauri EXE 已成功启动，可读取 `http://tauri.localhost/` 的登录界面；此前启动超时不再作为当前启动结论。
- 实测发现关闭到托盘后再次运行 EXE 不能唤回窗口。加入 Tauri 官方 single-instance 插件，调用已有 `show_main`；原有业务互斥保护保留。
- 修复后的 release 构建与 `cargo check --offline -p desktop-host`、格式检查通过。真实回归：窗口 38667718 隐藏后再次启动重新出现，始终为同一 PID 13604，只有一个客户端进程。
- 当前本地候选 `dist/Superkiro.exe`：12,520,448 字节；SHA256 `E6DFB2D0691806C5DE41E3FAEEF045AFBF62F1589355ACBAD409C69112E0B4A1`。未替换公开下载。
- 六个线上发布模型路由实际返回 OK，均产生积分扣除；相同调用 ID 重放无重复扣费。使用临时卡，结束后封禁且令牌返回 401；未消耗用户验收卡。此项仅证明线上协议/API链路，不等于真实 Kiro GUI 验收，也不独立证明上游模型身份。
- 本机只读检测识别 Kiro 1.1.14；未运行时内存采样为 0，没有配置快照，本次未修改 Kiro 配置或证书。
- 原生截图报 `SetIsBorderRequired / 0x80004002`，鼠标输入报 `coordinate input geometry is unavailable`。辅助功能读取与窗口关闭/唤回可用，但不能据此判定原生视觉或全流程交互通过。
- 原生卡密登录、手动启用、真实 Kiro 模型选择/聊天、恢复以及像素级设计一致性仍未验收。生产 `/admin/` 仍返回 Basic challenge，新管理表单登录迁移未部署。
- 完整分项结果见 `.acceptance/native-live-acceptance.json`；真实模型结果见 `.acceptance/six-model-deployed-results.json`。


## 最新发布验收：native.2（2026-09-19）

- 生产候选 `20260919T070809Z` 已上线，网关与 Caddy 健康。管理入口 `https://kiro.rent/admin/` 为网页表单登录，不再弹 Basic 对话框，也不要求输入原始管理员密钥。
- 停机一致性备份后完成旧/新镜像隔离恢复演练：33 张既有卡密余额、绑定、状态与可恢复性一致；既有加密卡密及新生成卡密均通过重启后精确读取。既有 KEK、上游密钥未轮换。
- 首次演练因 Docker internal 网络不提供宿主机端口映射，以及旧认证 Origin 与隔离主机不匹配而失败，已自动回滚旧版。修复演练脚本后，重新制作一致性备份并完整重跑成功，未沿用旧演练标记。
- 新 Cookie 备份脚本已安装；生产 systemd 实际运行 `Result=success`、`ExecMainStatus=0`，备份 timer 已恢复 active。原 runner 与前次备份保留。
- 真实公网 API 验收：网页无 Basic challenge；原始管理员密钥拒绝；Cookie Secure/HttpOnly/Strict、CSRF；PRO 1000 积分单设备卡；管理员明文读取 no-store；查询不绑定设备；第二设备被拒绝。
- 六个已发布路由均真实回复 OK、扣除积分，重复调用不重复扣费；临时卡完成后封禁，旧令牌拒绝；管理员退出后旧 Cookie 失效。此结果证明已配置上游路由可用，不独立验证上游宣传的模型身份。未消耗用户验收卡。
- 真实 Chromium 验收：官网、文档、换绑页在 1440/375 宽度无水平溢出；查询/确认换绑成功；管理表单登录、明文读取、九个管理页面导航、退出均通过。不代表各页每项业务操作全部验收。
- 桌面 27/27 测试与最终 release 构建通过。标题栏使用 Tauri 官方 `data-tauri-drag-region`，品牌与空白可拖动，交互控件不参与；最小权限已嵌入最终 EXE。
- 动效包含金属反光、装饰光栅、页面/弹窗入场、按钮反馈、真实待处理指示；尊重 reduced-motion。独立复核发现的 backdrop 漏项已修复，并验证 computed animation=none、transition=0s。
- 最终前端 JS `index-9HCQlw1u.js`，CSS `index-CesuRe4b.css`。Windows 单文件 `Superkiro-20260919-native.2-windows-x64.exe` 已上传，公开 manifest 已更新，完整公网下载 SHA256 验证通过。
- SHA256：`39b124a029363ddf63165f79d7b8aeddf88cdf731d386ed34f0720154cf0cce1`。未签名；需要系统 WebView2；不是 Python 软件，也不是安装器。
- 本地 `dist/Superkiro.exe` 已替换并成功启动；本轮最终包 PID 9248、窗口 51055110，关闭到托盘后再次启动仍唤回同一窗口与进程。

### 尚未验证或未实现

- 原生截图仍被 Windows `SetIsBorderRequired / 0x80004002` 阻断，无法进行可信的鼠标坐标拖动验收。浏览器官方拖动脚本/事件测试不等于真实窗口位移验证。
- 最终包真实 Kiro GUI 的卡密登录→手动连接→模型选择→聊天→恢复，以及原生像素级设计一致性，仍不能标记全流程通过。
- 原生 macOS 未构建验收、未签名公证。公开 manifest 已撤下旧 Python Mac 包，不将其冒充本次原生版本。
- TOTP、部署级 RLS、完整业务财务核算、持久安全审计仍未完成。管理登录全局速率限制仍有被匿名流量耗尽的可用性加固空间。
- 本次为可下载测试版，不宣称已完成商用品质或全部功能验收。

证据：`deployment-candidate-results.json`、`.acceptance/native-release-live.json`、`.acceptance/domain-browser/results.json`、`.acceptance/native-final-verification.json`、`.acceptance/native-published.json`、`.acceptance/native-final-build.log`。

## 本地修复候选：等待用户实测（2026-09-19）

本节为最新状态，优先于上方已发布版本的说明。本轮不部署、不替换公网下载、不提交或推送 GitHub。

- 桌面固定圆角窗口外壳，标题栏固定，仅内部内容滚动。错误、notice、恢复状态不会撑破外框。39 项前端测试与 30 个浏览器布局场景通过；缩小视口不等于 Windows 原生 DPI 或像素级验收。
- 管理入口改为独立登录页：checking/unauthenticated 不挂载后台、不请求敏感数据；401/退出卸载后台并清理会话，在途请求通过 abort 和版本屏障隔离。五组本地契约与浏览器测试、构建通过。生产网站仍是原发布版本。
- 真实 Kiro 安装使用同一安装内的 Electron 做语法检查，不要求用户安装 Node；保留解析门禁及禁用 RunAsNode 的拒绝路径。Windows 空 PATH 真实安装检查通过，真实扩展未修改。
- 连接失败新增六个阶段码，前端仅显示中文脱敏提示；失败后重新读状态，存在待恢复或状态不明确时禁止重复配置。
- Debug 启动入口改为构建并运行 target/debug/Superkiro.exe；不再启动 Python 界面。原“启动桌面客户端”入口转到同一 Debug 脚本，避免误测旧客户端。
- desktop-host 11 项测试通过，显式只读 installed_kiro_connection_preflight 通过，Rust/Tauri Debug 编译通过。前端产物 index-Cp1W8UcI.js / index-Csz4evVi.css。
- 尚待用户验证：实际卡密登录、手动启用、Kiro 重启、模型选择、聊天回复、积分扣减与恢复；原生拖动、显示缩放、圆角和动效。本轮未消耗用户卡密，未将这些步骤标记通过。

证据：.acceptance/local-host-tests.log、.acceptance/local-debug-preflight.log、.acceptance/local-debug-build.log、apps/desktop-ui/verification/layout/results.json、apps/admin-ui/ADMIN-ACCEPTANCE.md。
