# Kiro 1.0.437 真实登录态与回调分析报告 (P0-4 Auth)

> **实测环境**：Windows 11, Kiro 1.0.437, kiro.kiro-agent 1.0.794  
> **实测数据源**：`%USERPROFILE%\.aws\sso\cache\kiro-auth-token.json` 真实落盘文件 + `extension.js` `AuthSSOServer` 与 `TokenStorage` 源码追踪。  
> **对应设计项**：回答 Design Spec §16-3（登录态位置、schema、回调形态）与 §16-5（token 签发方校验）。

---

## 1. 登录态文件位置与 Schema (回答 Spec §16-3)

### 1.1 存储位置
在 Kiro 1.0.x 大版本中，登录凭据**继续沿用**经典 SSO 缓存路径：
* **Windows**: `%USERPROFILE%\.aws\sso\cache\kiro-auth-token.json`
* **macOS / Linux**: `~/.aws/sso/cache/kiro-auth-token.json`

在 `extension.js` 中确凿的代码定义为：
```javascript
ixd = "kiro-auth-token.json";
cacheDirectory = K3r.join(Ses.homedir(), ".aws", "sso", "cache");
getAuthTokenPath() { return K3r.join(this.cacheDirectory, ixd); }
```

### 1.2 数据格式 Schema
真实抓取脱敏样本（参见 [`docs/p0/samples/auth_token_sample.json`](./samples/auth_token_sample.json)）：
```json
{
  "accessToken": "aoaAAAAAGqiTE0y...",
  "refreshToken": "aorAAAAAGsXiEcs...",
  "profileArn": "arn:aws:codewhisperer:us-east-1:699475941385:profile/EHGA3GRVQMUK",
  "expiresAt": "2026-09-10T06:21:02.273Z",
  "authMethod": "social",
  "provider": "Google"
}
```

| 字段名 | 类型 | 说明 | 网关/客户端对接要求 |
| :--- | :--- | :--- | :--- |
| `accessToken` | string | 请求 Bearer Token | 网关签发的短时 JWT（承载 `card_id`, `group_id`, `token_version`） |
| `refreshToken` | string | 刷新 Token | 网关签发的刷新凭证 |
| `profileArn` | string | AWS CodeWhisperer Profile ARN | 保持合法 ARN 格式（如 `arn:aws:codewhisperer:us-east-1:123456789012:profile/BYOK`） |
| `expiresAt` | string (ISO-8601) | 过期时间戳 | 设为当前时间 + Token TTL（如 1 小时） |
| `authMethod` | string | 认证模式枚举 | 必须为 `"social"` 或 `"IdC"`（Social 走个人凭据，推荐固定为 `"social"`） |
| `provider` | string | 身份提供商名称 | 推荐使用 `"Google"` 或 `"Github"`（在 `sxd` 白名单中） |

### 1.3 核心发现：原生热重载支持 (Hot Reloading)
`TokenStorage` 对该文件注册了 `fs.watchFile` 监听：
```javascript
this.watchListener = () => {
  let r = this.tokenCache;
  this.clearCache();
  let n = this.readTokenFromDisk();
  this._onDidChange.fire({ oldToken: r, newToken: n });
};
Np.watchFile(e, this.watchListener);
```
**重大价值**：外部桌面客户端（Tauri）在卡密登录成功后，**直接写磁盘文件 `kiro-auth-token.json`，Kiro 无需重启进程即可热感知新登录态！**

---

## 2. 登录回调形态与 PKCE (回答 Spec §16-3)

Kiro 内部的 `AuthSSOServer` 采用标准 OAuth 2.0 Authorization Code + 回调流程：

1. **本地监听服务**：
   * 绑定地址：`http://127.0.0.1:<random_port>`（超时时间 10 秒，若冲突则端口自增）；
   * 回调路径：固定为 `/oauth/callback`。
2. **外部授权请求**：
   * 浏览器打开门户：`${PORTAL_URL}/signin?redirect_uri=http://127.0.0.1:<port>/oauth/callback&state=<state>`；
3. **回调入参处理**：
   * 提取 Query 参数：`code` 与 `state`；
   * 校验 `state` 防重放与 CSRF；
   * 成功响应：HTTP 302 重定向至 `${PORTAL_URL}/signin?auth_status=success&redirect_from=KiroIDE`；
   * 失败响应：HTTP 302 重定向至 `${PORTAL_URL}/signin?auth_status=error&redirect_from=KiroIDE&error_message=<err>`。
4. **客户端桌面端免网页登录方案**：
   由于我们拥有桌面客户端，且明确了 `kiro-auth-token.json` 支持直接写入与文件监听热加载，**桌面客户端可直接完成“卡密 -> 网关换 Token -> 写入本地文件”的一键静默登录，甚至无需启动浏览器网页交互！**

---

## 3. Token 签发方校验分析 (回答 Spec §16-5)

在 `extension.js` 中检索并逆向了全部 Token 使用点：
1. **客户端完全不校验 Token 签名**：
   * 代码读取 `kiro-auth-token.json` 时仅执行 `JSON.parse(r)`，没有任何公钥证书、JWKS、Issuer 校验逻辑；
   * 客户端在向后端发起请求时，仅将 `accessToken` 原样拼装到请求头 `Authorization: Bearer <accessToken>` 中；
2. **结论**：
   * 网关可以使用自建的密钥对签发标准的 JWT，完全不受官方签名算法限制；
   * JWT 内嵌入 `card_id`、`group_id` 与 `token_version`，网关在每次请求拦截时比对内存中的版本号，即可实现毫秒级的即时吊销（封号/冻结立即生效）。

---

## 4. `KIRO_MACHINE_TOKEN` 作用核实

逆向确认：`UOl = { external_idp: "EXTERNAL_IDP", machine_token: "KIRO_MACHINE_TOKEN", api_key: "API_KEY", IdC: "SSO_OIDC" }` 为客户端内部用于请求头埋点 `x-amzn-kiro-client-attribution` 的遥测枚举标识，并非本地存储的登录凭证。
