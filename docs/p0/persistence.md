# Kiro 接管持久性方案定夺报告 (P0-3 Persistence)

> **实测环境**：Windows 11, Kiro 1.0.437, macOS / Linux 规范对照  
> **对应设计项**：回答 Design Spec §16-2（接管持久性方案拍板：快捷方式劫持 vs 补丁兜底）。

---

## 1. 方案对比与决选结论

在解决“用户双击原生桌面图标或通过其他方式启动 Kiro 是否绕过接管”的持久性问题上，我们对两种候选方案进行了深入推演与实机比对：

| 维度 | 方案 A：快捷方式劫持 (Shortcut Hijacking) | 方案 B：Settings 配置持久化 + 扩展补丁兜底 (推荐) |
| :--- | :--- | :--- |
| **覆盖启动入口** | **低**：仅覆盖通过修改后的快捷方式双击启动。从任务栏固定图标、命令行 `kiro .`、右键菜单“Open with Kiro”、Git 外部编辑器等入口启动均会**彻底失效并穿透回官方直连**。 | **全覆盖 (100%)**：无论从快捷方式、任务栏、终端 CLI、系统关联打开，均全局生效。 |
| **跨平台一致性** | **差**：macOS 的 Dock 与 Spotlight 通过 LaunchServices 直接调起 `.app` 二进制，不执行快捷方式；Linux 终端启动不读 `.desktop`。 | **极佳**：VS Code 的 `User/settings.json` 与插件扩展目录机制在 Windows/macOS/Linux 完全一致。 |
| **反作弊/签名影响** | 无修改，不触碰签名。 | **无影响**：经核实，`extensions/kiro.kiro-agent/dist/extension.js` **不在 `product.json` 的 `checksums` 完整性哈希校验清单中**，打补丁绝不触发损坏弹窗。 |
| **升级抗性** | Kiro 重新生成快捷方式时可能覆盖参数。 | Kiro 静默更新会覆盖 `extension.js`。**对策**：在 `settings.json` 中配置 `update.mode: "none"` 锁定静默升级，并由客户端 Doctor 进行版本感知与一键重打。 |
| **卸载与回滚干净度** | 需还原 `.lnk` 目标路径。 | **精准无痕回滚**：只删除我们添加的 settings 键，并由 `extension.js.kpatch-backup` 恢复原文件。 |

### 最终拍板决策
**选定方案 B：`settings.json` 全局配置持久化 + `extension.js` 精准标记补丁兜底。**  
放弃单纯依赖桌面快捷方式劫持的脆弱方案。

---

## 2. 各平台落地机制规范

### 2.1 全局配置持久化（第一道防线：覆盖大部分管理与认证面）
所有平台统一在 Kiro 的 `User/settings.json` 中安全合并以下键值（卸载时精确移除，不影响用户原有配置）：
```json
{
  "kiroAuthConfig": {
    "portalUrl": "https://<GATEWAY_HOST>",
    "endpoint": "https://<GATEWAY_HOST>"
  },
  "codewhisperer.config": {
    "krsEndpoints": [{ "region": "us-east-1", "endpoint": "https://<GATEWAY_HOST>" }],
    "cpsEndpoints": [{ "region": "us-east-1", "endpoint": "https://<GATEWAY_HOST>" }],
    "endpoints": [{ "region": "us-east-1", "endpoint": "https://<GATEWAY_HOST>" }]
  },
  "kiroAgent.enableTabAutocomplete": false,
  "update.mode": "none"
}
```
* **Windows**: `%APPDATA%\Kiro\User\settings.json`
* **macOS**: `~/Library/Application Support/Kiro/User/settings.json`
* **Linux**: `~/.config/Kiro/User/settings.json`

### 2.2 扩展精准标记补丁（第二道防线：覆盖硬编码的 `K2u` 主对话面）
由于 `acp-q-client` 在构造时传入了 `endpoint: p`，会屏蔽 `AWS_ENDPOINT_URL` 环境变量，必须对 `extension.js` 执行精准微补丁：
1. **备份机制**：修改前对目标文件生成 `<path>.kpatch-backup`；
2. **幂等标记**：文件顶部写入 `/* @patched-kiro-byok v1 */`，检测到标记则跳过，绝不重复补丁；
3. **精准替换**：
   * 将 `K2u=c(t=>`https://runtime.${t}.kiro.dev``` 替换为动态读取环境变量或默认指向本地网关的函数；
   * 将 `J2u` 与 `Cru` 同步统一；
4. **运行态锁保护**：打补丁前检测 Kiro 进程是否存活，若存活则提示用户关闭或自动辅助重启，严禁在文件被占用时强制写入导致损坏。

---

## 3. 破坏场景实测与防护（审计门 B 验证）

### 场景 1：用户卸载/一键恢复官方直连
* **动作**：桌面客户端触发“恢复官方直连”；
* **执行**：
  1. 读取 `extension.js.kpatch-backup` 还原 `extension.js` 并删除备份文件；
  2. 从 `settings.json` 中移除 `kiroAuthConfig`、`codewhisperer.config` 以及由我们管理的特定键，恢复 `update.mode`；
* **结果**：Kiro 恢复为 100% 原厂状态，无残留、无报错。

### 场景 2：Kiro 被用户手动升级或覆盖安装
* **现象**：新安装包覆盖了 `extension.js`，原补丁失效；
* **检测**：桌面客户端在启动时（或守护线程中）检查 `extension.js` 首行是否存在 `/* @patched-kiro-byok */` 标记及当前版本；
* **修复**：若未发现标记，UI 状态即刻显示“接管已失效（检测到 Kiro 升级）”，并提供“一键修复”，重新应用补丁并更新备份，实现热修复。
