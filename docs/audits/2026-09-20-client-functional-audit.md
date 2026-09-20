# 客户端功能与操作流程独立审计（2026-09-20）

## 范围与方法

审查 `apps/desktop-ui` → `crates/desktop-host` → `crates/patch-engine` 的当前原生调用链，读取 README、原生迁移/功能验收指引与 ponytail skill。以实际用户动作逐步追踪状态、IPC、进程关闭、凭据与恢复文件，不将既有测试通过视为功能完整的证明。

未操作用户实际 IDE、未启动客户端原生窗口、未连接生产接口、未部署；新增网络回归仅使用 loopback HTTP/HTTPS fixture。没有修改 admin/gateway/billing。工作区出现的这些范围内并行改动不属于本审计。

## 确定问题与修复

### C1 / 高：仅验证卡密也可能结束未被接管的官方 Kiro（已修复）

复现路径：启动客户端 → 验证卡密但不启用连接 → 账户设置 → 切换卡密，或触发还原。前端原先仍调用 `/api/restore`；宿主无条件进入 `stop_kiro_for_restore`。此停止流程有强制结束回退，因此即使没有快照/接管会话，也可能关闭官方 IDE 并丢失未保存内容。确认框不是执行无必要破坏性操作的理由。

修复：宿主只有存在恢复状态才调用停止回调；引擎无会话且无快照的恢复直接成功，不检查或停止实际进程；前端在明确无恢复状态时显示“不关闭 Kiro、不修改官方配置”。未知/损坏状态不按空状态处理。

路径：`crates/desktop-host/src/backend.rs`、`crates/patch-engine/src/desktop.rs`、`apps/desktop-ui/src/App.tsx`。回归使用 panic 停止回调证明空恢复不会调用停止流程，前端覆盖只验证后的切卡和解绑定。

### C2 / 中：恢复后仍显示解除设备绑定，但操作必然因会话已删除而失败（已修复）

复现路径：启用连接 → 还原 Kiro → 当前卡密仍留在概览 → 账户设置 → 解除设备绑定。还原会删除本地 DesktopSession，云端绑定并不随之解除；原 unbind 强制读取 Session，无法完成操作，且此前会先关闭 Kiro。当前设备已经建立云端绑定但没有本地会话时，仅验证登录也遇到同类入口失效。

修复：新增 `DesktopSession::unbind_with_gateway`，存在会话时仍使用持久化 gateway/device/CA，无会话时使用前端明确传入并由宿主校验的 gateway、宿主 CA 和当前设备指纹。无本地恢复状态不停止 IDE、不创建接管会话，不删除或改写官方令牌。损坏会话拒绝绕过。宿主解绑定入口初始化应用级 CA；现有 CLI `unbind` 的会话语义不变。

路径：`crates/patch-engine/src/desktop.rs`、`crates/desktop-host/src/backend.rs`。新增本地 HTTP fixture 验证请求卡密/设备/挑战字段、令牌字节不变、无新增快照；补充现有隔离 HTTPS 子进程回归，验证恢复后仍使用宿主 CA。没有改变服务端解绑定政策。

## 按流程检查

| 用户步骤 | 代码核查结果 | 证据与边界 |
| --- | --- | --- |
| 首次打开 | 状态检测、安全存储读取、无卡密恢复入口存在；无 IDE 时可进入选择安装位置流程 | App 初始化/登录/设置，Host status/pick；未做原生文件选择器验收 |
| 验证卡密 | verify-card 只查询授权，不写 IDE；启用连接为另一步显式确认 | Host verify_card、App login/ask；网络/无效/到期状态有错误呈现 |
| 连接 | 预检、启动准备、云端认证、关闭、备份写入、启动分阶段；启动失败保留恢复状态 | DesktopSession activate_and_launch；认证可能先建立云端绑定，再遇到关闭取消，这是 C2 必须支持无会话解绑定的原因 |
| 连接与余额展示 | 配置应用不冒充真实模型已验证；使用服务端 availableCredits，不编造美元价格 | App/bridge、Host usage_output；真实模型请求未验证 |
| 正常恢复 | 补丁哈希验证在回滚前；令牌保留原字节；只恢复托管设置并保留用户新增配置 | SnapshotManager、ExtensionPatcher、SettingsManager、DesktopSession |
| 恢复后继续操作 | 保留已确认卡密展示与余额，重新连接可再次认证；切卡/解绑定覆盖新增回归 | 不把保留 UI 会话等同于仍有本地接管会话 |
| 超时/错误恢复 | 宿主持有串行操作锁；前端超时后查询 operation，不盲目重放写入；备份不由前端删除 | backend run_operation / App mutate+reconcile；宿主永久失联仍需重启客户端 |
| Kiro 升级 | 检测未知扩展内容后拒绝旧备份覆盖，安全策略正确，但没有完整用户恢复出口 | 见 C3，不能按“有恢复按钮”算流程完整 |
| 客户端升级 | 已补版本注入读取及手动下载入口、先还原再升级说明 | 见 C4；未实现自动更新 |
| Windows/macOS | 安装探测分平台，macOS 要求 Kiro.app；内存整理仅 Windows，macOS 仅监测；凭据使用平台安全存储 | macOS 原生对话框、Keychain、签名与真机接入未验收 |
| 关闭/退出 | 窗口关闭可托盘/最小化/请求还原退出；显式退出和托盘退出有恢复保护 | 应用级 OS Quit 与窗口 CloseRequested 并非同一事件，见剩余风险 |

## 后续复核与剩余边界

### C3 / 中：升级或扩展内容改变后的恢复流程形成死路

`ExtensionPatcher::restore_material` 对未知当前哈希返回 ExtensionChanged 并保留备份。`SnapshotManager::restore_official` 在恢复设置前执行该预检，因而设置/令牌/快照仍待恢复。前端仅提供重复恢复与诊断，原生显式退出又要求没有 recovery_pending；用户不能仅靠已有正常流程完成恢复和退出。

这是恢复产品流程缺口，不是建议去掉哈希保护。已在恢复失败页补充可执行支持步骤：保存工作并暂停升级/重装，保留 snapshot、扩展备份及 session（含凭据，不公开上传），进入连接诊断重新检测并导出脱敏报告，记录两端版本和失败时间，通过官网支持渠道确认匹配恢复方案。受控恢复策略与真实 IDE 升级恢复仍未验收。本轮没有覆盖升级文件、清除恢复标记或提供绕过安全校验的按钮。

### C4 / 中：版本不可区分 preview，缺少升级入口与说明（已补最小入口，构建注入协作完成）

原 `/api/status.app_version` 使用 `CARGO_PKG_VERSION`，只能展示 0.1.0，无法区分仓库发布记录中的 preview。按用户追加要求，status 改用 `env!("SUPERKIRO_BUILD_VERSION")`；build.rs/CI 由协作方修改，读取 `SUPERKIRO_BUILD_REVISION`，生成 `0.1.0-preview.<前12位sha>`，本地无 revision 时回退 0.1.0。

账户设置增加“下载新版”，仅通过系统浏览器打开 `https://kiro.rent/#downloads`，并提示保存工作、成功还原配置、退出后安装、重新验证版本；还原失败先保留备份并诊断。没有引入自动检查、版本比较、下载安装或更新签名验证，不声称完成安装包升级验收。

### C5 / 高：无会话解除绑定可能向默认网关发送自定义网关卡密（已修复）

前端传递验证过的 gateway，重启载入的 session gateway 在本次恢复后仍保留；宿主无会话时校验 body.gateway_url，有会话则持久化 gateway 优先。测试覆盖自定义地址、保存值优先、危险 URL 拒绝、验证后及重启恢复后解绑定。没有改服务端接口。

### C6 / 中：认证失败丢弃类别与重试时间（已修复）

AuthClient 的登录、刷新、解绑定统一读取限长错误 JSON，只识别白名单 code / __type，转换为固定安全文案；Retry-After 仅保留 0..86400 的整数秒。不透传任意 message、未知 code 或原始 header。覆盖 UnrecognizedClientException、ExpiredTokenException、AccessDeniedException 等新旧契约。UI 在 connection 通用错误之前显示安全类别及重试说明。

边界：不解析 Retry-After 的 HTTP-date 形式；历史 operation 诊断仍仅保留通用 auth-rejected 与 HTTP 状态。

### C7 / 中：usage 明确拒绝后仍声称已认证（已修复）

refreshToken 与 getUsageLimits 返回 401/403 时持久标记 authenticated=false，但保留 session、原凭据和官方令牌恢复备份；429、503、网络错误不使授权失效。UI 用量失败后刷新本地状态。loopback fixture 覆盖两条路径各五种结果、重新加载状态及失效后的本地拒绝。

## 其他剩余边界

- 原生宿主拦截 WindowEvent::CloseRequested，但未注册 RunEvent::ExitRequested 处理；因此代码尚不能保证 macOS 应用级 Quit 走同一还原确认路径。窗口关闭测试不能覆盖此路径，需原生 macOS 验证，不声称已经实测复现。
- 恢复停止流程与连接关闭策略不同：连接使用温和关闭；确认恢复可强制结束。UI 已警告未保存数据风险；不要引用旧 README 的“Mac 永不强杀”概括当前所有恢复路径。
- macOS .app 选择器、凭据系统拒绝访问、托盘实际可用性、下载导出行为、签名/公证和原生窗口行为仍需平台验收；Linux 非本轮发布验收目标。
- 真正模型请求、服务端设备绑定政策、生产计费与服务稳定性不在本次本地 fixture 证明范围内。
- 强制结束应用/系统断电仍依赖持久恢复记录；本轮没有模拟实际系统断电或用户 IDE 升级。

## 测试记录

- `npm --prefix apps/desktop-ui test`：108 通过（新增 2 项），3 个测试文件通过。
- `npm --prefix apps/desktop-ui run build`：TypeScript 检查与 Vite 生产构建通过。
- `cargo test --locked -p patch-engine --lib`：首轮 55 通过、1 忽略、0 失败；均未启动用户 IDE。
- 前轮最终 `patch-engine --lib desktop::`：9 通过，包含 HTTPS CA 复测；`desktop-host`：33 通过、1 忽略。
- 追加契约修复与升级入口的最终定向结果见本节追加记录。

仅修改以上客户端文件和本报告。日志保存在工作区 `client-audit-*.log`，不是生产验收记录。

### 追加契约测试结果

- `cargo test --locked -p patch-engine --lib client_contract`：2 通过，其中 usage/refresh fixture 覆盖 10 个分支。首跑仅临时锁文件清理失败，修正 fixture 后复测全部通过。
- `cargo test --locked -p desktop-host client_contract`：1 通过，已编译验证 SUPERKIRO_BUILD_VERSION 注入。
- 最终前端回归（含升级入口、preview 展示、重启恢复后的网关保留和支持指引）：112 通过，3 个文件通过；TypeScript/Vite 构建通过。
- scoped `git diff --check` 与 `cargo fmt -p desktop-host -p patch-engine -- --check` 通过。

本轮启动的所有命令会话均已结束；未结束其他代理或用户进程。Rust 追加验证仅运行上述 client_contract 定向过滤，没有运行 workspace 全量测试。
