# Kiro 1.0.437 / Agent 1.0.794 端点改道覆盖面实测报告 (P0-2 Coverage)

> **实测环境**：Windows 11, Kiro IDE 1.0.437 (commit 5349479558), kiro.kiro-agent 1.0.794  
> **验证依据**：本地 12.9MB 真实 bundle `extension.js` 全量 AST 逆向与符号追踪，结合 `state.vscdb`、`kiro-auth-token.json` 实测取样。  
> **对应设计项**：回答 Design Spec §16-1（env 改道覆盖面）。

---

## 1. 核心结论摘要

1. **纯环境变量无法实现 100% 数据面改道**：
   - 官方虽然在代码中内嵌了标准 AWS SDK 的 `AWS_ENDPOINT_URL` 解析逻辑，但在客户端实例化关键服务（如 `acp-q-client`、`CodeWhispererStreaming`、`CodeWhispererRuntimeClient`）时，构造函数参数显式传入了 `endpoint: p`（`K2u(d)` 或 `Hve()`）。
   - 根据 AWS SDK v3 的解析规则：**构造函数显式传入的 `endpoint` 参数优先级高于 `AWS_ENDPOINT_URL` 环境变量**，因此纯 `AWS_ENDPOINT_URL` 会被显式硬编码的 `https://runtime.us-east-1.kiro.dev` 覆盖，产生“以为改道了但实际上直连官方”的**假阳性危险**。
2. **VS Code Settings 提供了原生覆盖钩子**：
   - `extension.js` 中内置了非公开配置项读取：
     - `kiroAuthConfig.portalUrl` / `kiroAuthConfig.endpoint`
     - `codewhisperer.config.krsEndpoints`
     - `codewhisperer.config.cpsEndpoints`
     - `codewhisperer.config.endpoints`
3. **最佳接管方案（混合架构）**：
   - **登录门户**：`KIRO_AUTH_PORTAL_URL` 环境变量生效。
   - **设置写入**：通过 `settings.json` 注入 `codewhisperer.config.*` 与 `kiroAuthConfig.*`。
   - **补丁兜底**：对 `extension.js` 内的 `K2u` / `J2u` / `Cru`（均指向 `https://runtime.${t}.kiro.dev`）打正则热补丁，确保即便设置被覆盖也能 100% 劫持。

---

## 2. 接口面改道覆盖矩阵

| 接口面 | 官方默认域名与路径 | 环境变量生效情况 | Settings 覆盖情况 | 最终接管策略 |
| :--- | :--- | :--- | :--- | :--- |
| **登录认证门户** | `https://app.kiro.dev` | **命中** (`KIRO_AUTH_PORTAL_URL`) | 支持 (`kiroAuthConfig.portalUrl`) | **环境变量注入** `KIRO_AUTH_PORTAL_URL=https://<gw>` |
| **桌面认证 API** | `https://prod.us-east-1.auth.desktop.kiro.dev` | 未命中 (无对应 env) | **命中** (`kiroAuthConfig.endpoint`) | **Settings 注入** `kiroAuthConfig.endpoint` / 补丁 |
| **对话主面 (Runtime)** | `https://runtime.us-east-1.kiro.dev/generateAssistantResponse` | 未命中 (`AWS_ENDPOINT_URL` 被构造参数覆盖) | 部分命中 (`codewhisperer.config.krsEndpoints`) | **Settings 注入 + 扩展补丁兜底 (`K2u` 改写)** |
| **模型列表** | `https://management.us-east-1.kiro.dev/ListAvailableModels` | 未命中 (被 `Hve` 显式覆盖) | **命中** (`codewhisperer.config.cpsEndpoints`) | **Settings 注入** `codewhisperer.config.cpsEndpoints` |
| **额度查询** | `https://management.us-east-1.kiro.dev/getUsageLimits` | 未命中 | **命中** (`codewhisperer.config.cpsEndpoints`) | **Settings 注入** `codewhisperer.config.cpsEndpoints` |
| **套餐订阅** | `https://management.us-east-1.kiro.dev/listAvailableSubscriptions` | 未命中 | **命中** (`codewhisperer.config.cpsEndpoints`) | **Settings 注入** `codewhisperer.config.cpsEndpoints` |
| **Profile 查询** | `https://management.us-east-1.kiro.dev/ListAvailableProfiles` | 未命中 | **命中** (`codewhisperer.config.cpsEndpoints`) | **Settings 注入** `codewhisperer.config.cpsEndpoints` |
| **Tab 自动补全** | `GenerateCompletions` | 未命中 (无对应 env) | **命中** (`kiroAgent.enableTabAutocomplete: false`) | **Settings 注入关闭** |
| **会话标题 LLM** | `SessionTitleGeneration` | **命中** (`KIRO_DISABLE_SESSION_TITLE_LLM=true`) | - | **环境变量注入关闭** |
| **Session Recap** | `Recap` | **命中** (`KIRO_DISABLE_RECAP=true`) | - | **环境变量注入关闭** |
| **代码库 Embeddings** | 本地运行 (`all-MiniLM-L6-v2`) | 无需改道 | - | 本地内置模型，不消耗网络 |
| **遥测 Telemetry** | `telemetry.desktop.kiro.dev` | 可配置 `OTEL_EXPORTER_OTLP_ENDPOINT` | 支持 `telemetry.telemetryLevel: "off"` | **Settings 注入关闭** |

---

## 3. 证据链条（Bundle 源码位置与符号证据）

### 3.1 登录门户：`KIRO_AUTH_PORTAL_URL`
在 `extension.js` 第 9436671 字符处：
```javascript
function hQe() {
  let t = process.env[cId]; // cId = "KIRO_AUTH_PORTAL_URL"
  if (t) return t;
  let e = _Xi.workspace.getConfiguration().get("kiroAuthConfig.portalUrl");
  return e || aId; // aId = "https://app.kiro.dev"
}
```
**结论**：`KIRO_AUTH_PORTAL_URL` 优先级最高，可 100% 环境变量改道。

### 3.2 桌面认证端点：`kiroAuthConfig.endpoint`
在 `extension.js` 第 9432328 字符处：
```javascript
function JCd() {
  let t = ove.workspace.getConfiguration().get("kiroAuthConfig");
  if (t) {
    if (t.endpoint) return t;
    ove.window.showErrorMessage("Invalid Kiro Auth configuration, please specify an endpoint");
  }
  return YCd; // YCd = { endpoint: "https://prod.us-east-1.auth.desktop.kiro.dev" }
}
```
**结论**：官方在此处未读取环境变量，只读取 `kiroAuthConfig.endpoint`。

### 3.3 运行时与管理端点：`codewhisperer.config.*`
在 `extension.js` 第 9697387 字符处：
```javascript
function IVr(t) {
  let e = SVr.workspace.getConfiguration("codewhisperer.config");
  return rPd(e.inspect(t), e.get(t), SVr.workspace.isTrusted);
}
pne = IVr("endpoints");
TVr = IVr("krsEndpoints");
xVr = IVr("cpsEndpoints");
```
调用链分析：
- `Gve()` -> `Los(Ios, TVr, t)`：负责 KRS (Kiro Runtime Service，即 `runtime.us-east-1.kiro.dev`)；
- `Hve()` -> `Los(Tos, xVr, t)`：负责 CPS (CodeWhisperer / Management Service，即 `management.us-east-1.kiro.dev`)。

### 3.4 对话主面 `acp-q-client` 硬编码：
在 `extension.js` 第 9699982 字符处：
```javascript
function jRi(t, e = {}) {
  // ...
  let d = e.region || t.region || W2u; // W2u = "us-east-1"
  let p = e.endpoint || K2u(d);       // K2u = (t) => `https://runtime.${t}.kiro.dev`
  // ...
  getKrsClient: async () => {
    let h = new Mme({
      region: d,
      endpoint: p, // 显式作为构造参数传入！
      token: { token: await t.getToken() },
      // ...
    });
  }
}
```
由于外部调用 `jRi` 时 `e.endpoint` 为 `undefined`，`p` 恒等于 `https://runtime.us-east-1.kiro.dev`。
在 AWS SDK v3 的 client 基类中：
```javascript
function LEl(t) {
  return (e) => {
    let { endpoint: n } = e;
    let l = Object.assign(e, {
      endpoint: n != null ? async () => wAr(await sd(n)()) : void 0,
      isCustomEndpoint: !!n,
    });
    return l;
  };
}
```
因为 `n`（即 `p`）不为空，`isCustomEndpoint` 被置为 `true`，**AWS SDK 不再去查询 `AWS_ENDPOINT_URL` 环境变量**。

---

## 4. 针对 Spec §16-1 的明确解答与实施指令

1. **能否仅靠环境变量完成改道？**
   - **答：不能。** 仅用 `AWS_ENDPOINT_URL` 环境变量会产生假阳性——看似配置了，但主对话请求仍会直接穿透打到官方 `https://runtime.us-east-1.kiro.dev`。
2. **必须配合的改道方式**：
   - 方式 A（配置注入）：在用户配置 `%APPDATA%\Kiro\User\settings.json` 中写入：
     ```json
     {
       "kiroAuthConfig": {
         "portalUrl": "https://localhost:8443",
         "endpoint": "https://localhost:8443"
       },
       "codewhisperer.config": {
         "krsEndpoints": [{ "region": "us-east-1", "endpoint": "https://localhost:8443" }],
         "cpsEndpoints": [{ "region": "us-east-1", "endpoint": "https://localhost:8443" }]
       },
       "kiroAgent.enableTabAutocomplete": false
     }
     ```
   - 方式 B（极简精准补丁）：在 `extension.js` 中将 `K2u=c(t=>`https://runtime.${t}.kiro.dev`` 改为读取网关地址（由于扩展不在 `product.json` 的 `checksums` 校验范围内，该补丁 100% 安全且零弹窗）。
   - 方式 C（透明代理 / hosts）：Windows 下将 `runtime.us-east-1.kiro.dev`、`management.us-east-1.kiro.dev` 解析至本地回环。
3. **Spec 修正建议**：
   - 客户端接管动作从原先的“纯启动环境变量注入”更新为“启动环境变量 + settings.json 安全合并注入 + 补丁兜底”。
