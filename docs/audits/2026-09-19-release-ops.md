# 发布与运维静态审计

基线：当前工作树，HEAD 6d1902e；大量未提交修改，不等同于该提交或线上版本。仅静态阅读，没有执行测试、构建、联网、发布或 GUI 操作。

## OPS-01 / P1：CI 管理端产物目录与测试读取目录不一致
- 证据：`.github/workflows/ci.yml:74` 构建到 `.build-check`；`apps/admin-ui/tests/visual.cjs:7`、`visual-authenticated.cjs:5` 固定读取 `../dist`。dist 被忽略，干净 checkout 不存在。
- 触发：GitHub 干净 runner 执行 frontend job。页面返回404，等待登录元素失败，而本机残留 dist 可能掩盖问题。
- 修复：统一构建输出和测试输入；认证门禁新测试 auth-session/auth-gate 同时纳入 CI。
- 验收：全新 checkout，无预存 dist，完成实际登录门禁与九页 fixture 测试。

## OPS-02 / P1：CI 桌面回归仍针对已删除的旧页面
- 证据：`.github/workflows/ci.yml:86` 执行 `test_superkiro_desktop_ui.py`；该文件 `:78-83` 只接受 desktop.js/desktop.css 并查询旧 DOM id。当前 `apps/desktop-ui/index.html:1` 引入 `/src/main.tsx`，旧 js/css 已删除。
- 触发：frontend CI 执行桌面测试，路由拦截拒绝新入口，旧选择器也无法工作。
- 修复：将当前 React/Vite 构建产物与 verify_layout.py/现行测试接入 CI，移除失效门禁。
- 验收：干净 runner 实际跑到当前 UI 的错误、恢复、连接状态，不仅执行历史 Python 桥测试。

## OPS-03 / P1：发布必需图标被 Git 忽略
- 证据：`.gitignore:40` 忽略全部 PNG；`crates/desktop-host/tauri.conf.json:44` 引用 icons/icon.png；git check-ignore 确认匹配，git ls-files 未纳入。
- 触发：后续提交新增 Rust/Tauri 源码但使用普通 git add，图标被遗漏，跨平台打包无法取得指定资源。
- 修复：为正式品牌资源添加精准忽略例外，提交必需资源；不要解除所有截图的忽略。
- 验收：干净 checkout 的 Windows/Mac 打包成功。当前原生宿主与React目录还存在未跟踪文件，需逐项整理提交清单，不能把本机工作树等同已发布源码。

## OPS-04 / P2：桌面发布脚本允许同一版本URL覆盖不同二进制
- 证据：`deploy/publish_native_windows.py:10-14` 固定版本、文件名并读取可变 target/release 文件，`:27` 直接替换公开文件，`:31` 再替换manifest；没有检查已存在版本内容一致或绑定验收产物摘要。
- 触发：重编后重复运行脚本，会在同一URL放置不同程序。并发执行还共用 .tmp 文件；发布时EXE与manifest存在不一致窗口。
- 影响：用户缓存、回溯、下载哈希与用户实测版本无法稳定关联。此项不表示本次执行了发布。
- 修复：版本/构建摘要唯一命名，拒绝覆写不同摘要，绑定明确的验收输入，发布加锁；成功后原子切换manifest。
- 验收：重复发布幂等、并发拒绝、保留旧URL、下载哈希与批准的产物一致。

## OPS-05 / P2：在线备份指南仍调用被关闭的旧鉴权
- 证据：`deploy/backup/README.md:10-24` 推荐 ADMIN_KEY + backup.sh；`backup.sh:59-60` 用 x-admin-key 申请session。生产模式 `crates/gateway/src/facade/admin.rs:88` 禁用 legacy，`admin_login.rs:44-46` 要求浏览器认证。已有 server-backup.py 使用Cookie新协议，但主指南没有切换。
- 触发：按现有备份文档操作新认证部署，在线同步失败；操作者可能错误使用 --force。
- 修复：主指南和运维入口统一到新runner，旧脚本标明只支持旧协议/冷备；补凭据权限、定时器、失败告警和恢复步骤。
- 验收：隔离环境照文档从零安装备份并恢复新runner包，核对账本、卡密和密钥，不在生产直接演练。

## OPS-06 / P2：部署文档建议账本目录世界可写
- 证据：`deploy/README.md:79` 推荐 chmod -R 777 ./data；compose使用UID1000非root服务挂载该目录。
- 触发：维护者照抄命令，本机其他账户可替换、删除账本/锚点，即便密文校验能阻止篡改，仍可造成拒绝服务或回滚风险。
- 修复：明确服务UID/GID所有权、目录0700/0750和文件0600/0640，禁止777。
- 验收：服务能读写，非授权本地用户不能修改或遍历敏感数据。

## 发布前仍需人工批准的边界
- 未签名Windows程序、未签名公证Mac产物与真实Mac交互不能标记为正式验收。
- 未运行新一轮依赖漏洞扫描或完整秘密扫描；本轮有限正则检查不能证明仓库无泄漏。
- 备份在同一服务器不等于异地灾备；是否有异地副本和恢复演练需独立检查部署状态。
