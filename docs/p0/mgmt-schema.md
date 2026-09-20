# Kiro 1.0.437 管理面响应 Schema 与 UI 渲染分析 (P0-5 Mgmt Schema)

> **实测环境**：Windows 11, Kiro 1.0.437, kiro.kiro-agent 1.0.794  
> **实测数据源**：`extension.js` 中 AWS Smithy CodeWhispererService 序列化元表，结合 `state.vscdb` 实测缓存与参考实现。  
> **对应设计项**：回答 Design Spec §16-6（管理面 4 个核心端点的响应 Schema 与最小必填集）。

---

## 1. 核心端点概览

Kiro 1.0.437 管理面由 `CodeWhispererRuntimeClient` 发起，其路径与方法如下：

| 端点操作 | HTTP 方法 | 官方请求路径 | 职责说明 |
| :--- | :--- | :--- | :--- |
| **`ListAvailableModels`** | `GET` | `/ListAvailableModels` | 获取可用模型列表及默认选定模型 |
| **`ListAvailableProfiles`** | `POST` | `/ListAvailableProfiles` | 查询凭据所属的 Profile ARN |
| **`ListAvailableSubscriptions`** | `POST` | `/listAvailableSubscriptions` | 查询套餐信息（Pro / Pro+ / Free） |
| **`GetUsageLimits`** | `GET` | `/getUsageLimits` | 获取积分余额、用量进度与账单重置日期 |

---

## 2. 接口详细 Schema 与 UI 渲染绑定

### 2.1 `ListAvailableModels` (模型列表)
* **样本文件**：[`docs/p0/samples/mgmt_list_available_models.json`](./samples/mgmt_list_available_models.json)
* **响应 Schema**：
  ```json
  {
    "models": [
      {
        "modelId": "claude-sonnet-4.5",
        "modelName": "Claude Sonnet 4.5",
        "description": "Balanced performance for coding",
        "tokenLimits": {
          "maxInputTokens": 200000,
          "maxOutputTokens": 64000
        }
      }
    ],
    "defaultModel": "claude-sonnet-4.5",
    "nextToken": null
  }
  ```
* **UI 绑定机制**：
  * Kiro Agent 聊天窗口顶部的模型选择下拉框直接遍历 `models` 数组；
  * 展示文本优先读取 `modelName`，若缺失则降级显示 `modelId`；
  * 下拉切换时写入当前激活的 `modelId`；
  * `tokenLimits.maxInputTokens` 与 `maxOutputTokens` 驱动客户端本地上下文窗口计算。
* **最小必填集**：
  `{ "models": [{ "modelId": "..." }] }`。缺 `models` 数组或 `modelId` 会导致下拉框空白或崩溃。

---

### 2.2 `ListAvailableProfiles` (Profile 查询)
* **样本文件**：[`docs/p0/samples/mgmt_list_available_profiles.json`](./samples/mgmt_list_available_profiles.json)
* **响应 Schema**：
  ```json
  {
    "profiles": [
      {
        "arn": "arn:aws:codewhisperer:us-east-1:123456789012:profile/KIRO_BYOK_DEFAULT",
        "profileName": "KiroProfile-us-east-1"
      }
    ],
    "nextToken": null
  }
  ```
* **UI 绑定机制**：
  * Kiro 扩展中的 `ProfileArnGuard` 会在启动时检查 `profiles[0].arn`；
  * 若返回空数组或首项无 `arn`，Kiro 会弹窗报警：`[ProfileArnGuard] No profiles available` 并阻断聊天；
  * 获取到 `arn` 后，会自动调用 `VC.getInstance().writeProfile({ arn, name })` 缓存。
* **最小必填集**：
  `{ "profiles": [{ "arn": "arn:aws:codewhisperer:us-east-1:123456789012:profile/DEFAULT" }] }`。必须包含至少一个带有合法 ARN 结构的元素。

---

### 2.3 `listAvailableSubscriptions` (套餐订阅)
* **样本文件**：[`docs/p0/samples/mgmt_list_available_subscriptions.json`](./samples/mgmt_list_available_subscriptions.json)
* **响应 Schema**：
  ```json
  {
    "subscriptionPlans": [
      {
        "qSubscriptionType": "PRO_PLUS",
        "description": "Kiro Pro+ Unlimited Plan",
        "pricing": { "currency": "USD", "amount": 0.0 }
      }
    ],
    "disclaimer": null
  }
  ```
* **UI 绑定机制**：
  * 驱动账号管理中心界面展示“当前套餐计划”；
  * `qSubscriptionType`（如 `"PRO_PLUS"`）作为虚拟化套餐的核心标识。
* **最小必填集**：
  `{ "subscriptionPlans": [{ "qSubscriptionType": "PRO_PLUS" }] }`。

---

### 2.4 `getUsageLimits` (用量与积分)
* **样本文件**：[`docs/p0/samples/mgmt_get_usage_limits.json`](./samples/mgmt_get_usage_limits.json)
* **响应 Schema**：
  ```json
  {
    "subscriptionInfo": {
      "subscriptionTitle": "KIRO PRO+",
      "type": "PRO_PLUS",
      "overageCapability": "ENABLED",
      "subscriptionManagementTarget": "MANAGE"
    },
    "usageBreakdownList": [
      {
        "displayName": "Credit",
        "displayNamePlural": "Credits",
        "currentUsage": 12.5,
        "currentUsageWithPrecision": 12.5,
        "usageLimit": 50000.0,
        "usageLimitWithPrecision": 50000.0,
        "currency": { "code": "USD", "symbol": "$" },
        "unit": "INVOCATIONS",
        "dimensionType": "CREDIT",
        "nextDateReset": 1790812800000
      }
    ],
    "overageConfiguration": {
      "overageStatus": "ENABLED",
      "overageEnabled": true
    },
    "userInfo": { "email": "user@kiro-byok.local" },
    "daysUntilReset": 30,
    "nextDateReset": 1790812800000
  }
  ```
* **UI 绑定机制（虚拟化核心杠杆）**：
  1. **套餐角标与名牌**：Kiro 原生侧边栏直接渲染 `subscriptionInfo.subscriptionTitle`（如 `"KIRO PRO+"`）；
  2. **积分进度条**：Kiro 状态栏或设置面板通过 `currentUsage / usageLimit` 计算百分比并显示剩余额度；
  3. **超额开关注入**：`overageConfiguration.overageStatus = "ENABLED"` 确保 Kiro 不会弹出“额度已用尽限制对话”的阻断卡片。
* **最小必填集**：
  ```json
  {
    "subscriptionInfo": { "subscriptionTitle": "KIRO PRO+" },
    "usageBreakdownList": [
      {
        "currentUsage": 0,
        "usageLimit": 1000,
        "displayName": "Credit",
        "displayNamePlural": "Credits"
      }
    ]
  }
  ```
  `usageLimit` 绝对不能为 0 或缺失，否则在前端执行除法时会出现 `NaN%` 或进度条异常碎裂。

---

## 3. 针对 Spec §16-6 的明确回答与设计定稿

1. **响应格式全部采用标准 AWS JSON 1.0**（`Content-Type: application/x-amz-json-1.1` 或 `application/json`）；
2. 4 个接口字段已全面证实并完成命名对齐；
3. **套餐/模型虚拟化实施路径完全畅通**：网关在收到请求后，从卡密所在分组读取 `virtual_plan_name`、`virtual_usage_limit` 及 `model_map`，即可构造出完美欺骗 Kiro 原生界面的响应体，让用户看到尊贵的 PRO+ 套餐与独立积分池。
