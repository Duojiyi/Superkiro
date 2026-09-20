# P0-7 辅助调用面控制与运行态检测技术报告

> **环境基线**：Kiro IDE 1.0.437 / Agent 1.0.794 (Windows x64)  
> **实测验证**：`product.json` 解析 + Win32 `OpenMutexW` 实时互斥量调用 + `extension.js` 逆向

---

## 1. 核心结论摘要 (回答 Spec §16-10, §16-11)

| 编号 | 议题 | 结论与代码实证 | 建议落地策略 |
|---|---|---|---|
| **§16-10** | autocomplete 关闭与"额度上限"错误期望形状 | **1. 关闭配置**：在 VSCode `settings.json` 设置 `"kiroAgent.enableTabAutocomplete": false` 即可关闭前端补全触发。<br>**2. 用户强制重开时的网关兜底响应**：<br>网关对 `GenerateCompletions` 端点返回 **HTTP 429**：<br>`{"__type": "ThrottlingException", "message": "Maximum Kiro usage reached for this month.", "reason": "MONTHLY_REQUEST_COUNT"}`<br>Kiro 捕获后触发官方原生提示："*Autocomplete Failed: Maximum Kiro usage reached for this month.*"，直接安全返回，不崩溃、不重试。 | 客户端安装/配置时向 `settings.json` 写入关闭项；网关实现该 429 错误响应。 |
| **§16-11** | Kiro 运行态可靠检测法 | **Win32 命名互斥量 + 进程检测双重保障**：<br>1. **Windows 原生互斥量**：`OpenMutexW(SYNCHRONIZE, FALSE, L"kiro")`。<br>实测证实：Kiro 运行时返回合法句柄（如 `handle=384`），退出后句柄为空且 `GetLastError()==2`。微秒级调用，零损耗。<br>2. **全平台进程快照**：检测进程名 `Kiro.exe` (Windows) / `Kiro` (macOS/Linux)。 | 桌面端/启动器先调用 `OpenMutexW`（Win），次选进程遍历，确保热修/改写配置前 Kiro 已完全退出。 |

---

## 2. 辅助 LLM 消耗面禁用实证 (`extension.js`)

为了防止隐式 LLM 调用成倍消耗第三方 API 费用，MVP 需关闭会话摘要、标题生成与 Tab 补全：

### 2.1 会话摘要 (Session Recap)
- **环境变量**：`KIRO_DISABLE_RECAP=true`
- **源码实证** (`extension.js#5290887`)：
  ```javascript
  async function qJl(t) {
    if (process.env.KIRO_DISABLE_RECAP === "true") {
      return { text: void 0, metrics: {} };
    }
    // ... 后续 LLM 生成调用
  }
  ```
- **生效机制**：检测到该环境变量后，函数首行直接返回 `text: undefined`，**100% 阻断任何网络与 LLM 请求**。

### 2.2 会话标题生成 (Session Title LLM)
- **环境变量**：`KIRO_DISABLE_SESSION_TITLE_LLM=true`
- **源码实证** (`extension.js#8707559`)：
  ```javascript
  kickoffLlmSessionTitle(e, r) {
    if (e.sessionTitleGenerationId = void 0,
        process.env.KIRO_DISABLE_SESSION_TITLE_LLM === "true" ||
        !e.featureConfig.get(ns.SESSION_TITLE_LLM)) {
      return;
    }
    // ... 后续 LLM 标题生成
  }
  ```
- **生效机制**：检测到该环境变量后，直接 `return`，不发起 `SessionTitleGeneration` 调用。

### 2.3 Tab 自动补全 (Tab Autocomplete)
- **配置项**：`"kiroAgent.enableTabAutocomplete": false`
- **源码实证** (`extension.js#12611333`, `#12621897`)：
  ```javascript
  var k3 = "kiroAgent", aoa = "enableTabAutocomplete";
  // 读取 vscode.workspace.getConfiguration("kiroAgent").get("enableTabAutocomplete")
  ```
- **生效机制**：写入 VSCode 全局 `settings.json`（`%APPDATA%\Kiro\User\settings.json`）后，Kiro 内部的补全监听器不再注册到编辑器。

---

## 3. GenerateCompletions 降级响应规范

当用户自行在 Kiro 设置中重新勾选开启 Tab 补全时，Kiro 会向网关发起 `GenerateCompletions` 请求。

### 3.1 客户端错误处理逻辑 (`extension.js#12625068`)
```javascript
if (g instanceof m1.AccessDeniedException)
    throw new tve("CodeWhispererRuntime: AccessDenied");
if (g instanceof m1.ThrottlingException && g.reason === "MONTHLY_REQUEST_COUNT") {
    lo.window.showErrorMessage("Autocomplete Failed: Maximum Kiro usage reached for this month.");
    return;
}
if (g instanceof m1.ValidationException) {
    C3.reportCountMetrics({ validationError: 1 });
    return;
}
if (g instanceof m1.ThrottlingException) {
    C3.reportCountMetrics({ throttled: 1 });
    return;
}
```

### 3.2 网关应当返回的响应报文
- **HTTP 状态码**：`429 Too Many Requests`
- **HTTP Headers**：
  ```http
  Content-Type: application/json
  x-amzn-errortype: ThrottlingException
  ```
- **HTTP 响应体**：
  ```json
  {
    "__type": "ThrottlingException",
    "message": "Maximum Kiro usage reached for this month.",
    "reason": "MONTHLY_REQUEST_COUNT"
  }
  ```
- **预期客户端效果**：VSCode 状态栏或通知栏显示 `"Autocomplete Failed: Maximum Kiro usage reached for this month."`，并优雅静默，不污染对话窗口。

---

## 4. Kiro 运行态检测方案对比与决议

### 4.1 方案对比

| 检测手段 | 实现原理 | 跨平台 | 性能开销 | 误报/漏报率 | 推荐等级 |
|---|---|---|---|---|---|
| **Win32 命名互斥量** | `OpenMutexW(SYNCHRONIZE, FALSE, "kiro")` | 仅 Windows | 极低 (<0.01ms) | 零（由 Electron 核心持有） | **首选 (Windows)** |
| **进程快照遍历** | 遍历系统进程查找 `Kiro.exe` / `Kiro` | 全平台 | 中 (1~5ms) | 低（除非被同名伪装） | **首选 (Mac/Linux) & 兜底** |
| **UserData 锁文件** | 尝试独占读写 `code.lock` 或 SingleInstanceSocket | 全平台 | 低 | 容易因非正常退出遗留死锁 | 不推荐作为单一依据 |

### 4.2 Windows 互斥量实测结果
`product.json` 中配置：
```json
"win32MutexName": "kiro",
"dataFolderName": ".kiro"
```
在 Kiro 正在运行的机器上执行 Win32 API：
```python
handle = ctypes.windll.kernel32.OpenMutexW(0x00100000, False, "kiro")
# 返回: handle = 384, GetLastError() = 0 (成功打开互斥体)
```
退出 Kiro 后：
```python
handle = ctypes.windll.kernel32.OpenMutexW(0x00100000, False, "kiro")
# 返回: handle = None, GetLastError() = 2 (ERROR_FILE_NOT_FOUND)
```
结论：Windows 下直接基于 `win32MutexName: kiro` 判定最为轻量且可靠。
