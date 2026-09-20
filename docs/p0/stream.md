# P0-6 对话 Event-Stream 逆向分析与语料规范报告

> **环境与版本基线**：Kiro IDE 1.0.437 / Agent 1.0.794 (Windows x64)  
> **验证代码**：`spikes/p0-stub/generate_stream_corpus.py` + `spikes/p0-stub/test_stream_corpus.js`  
> **语料目录**：`docs/p0/stream/*.bin`

---

## 1. 核心未知数解答 (Spec §16 决议)

| 未知数编号 | 议题 | 结论与实证事实 | 证据位置 |
|---|---|---|---|
| **§16-4** | event-stream 工具帧真实结构（分片 JSON、CRC） | **标准 AWS Event-Stream 二进制协议**：12 字节 Prelude (total_len, headers_len, prelude_crc32) + Headers (:event-type 等) + Payload (JSON) + 4 字节 message_crc32。<br>• `toolUseEvent` 字段：`toolUseId` (string), `name` (string), `input` (string，分片 JSON 字符串), `stop` (bool)。<br>• 客户端通过 `toolUseId` 归组分片 `input` 拼接为完整 JSON，当 `stop: true` 时结束该工具调用。 | `extension.js#5456197` (Iuu)<br>`extension.js#5487678` (tool_call_chunk)<br>`02_tool_use.bin` |
| **§16-7** | `sessionIdleTimeout` 实际取值 → 决定保活间隔 | **看门狗有两级阈值**：<br>1. 预警阈值 `warnMs`：默认 **60,000 ms (60秒)**，环境变量 `KIRO_STREAM_IDLE_WARN_MS`。<br>2. 超时阈值 `timeoutMs`：默认 **300,000 ms (300秒 = 5分钟)**，环境变量 `KIRO_STREAM_IDLE_TIMEOUT_MS`。<br>• 超过 60s 未收到任何帧触发 warning recovery 信号，超过 300s 抛出 `StreamIdleTimeoutError` 强制断流。<br>• **网关保活间隔策略**：必须严格在 60s 前发保活帧。**推荐每 20~25s 发一次空 `assistantResponseEvent` (`{"content": ""}`)**。该空帧被 UI 层静默忽略（`Iuu()` 过滤 `content.length > 0`），但会重置底层 `ghi()` 迭代器的 `setTimeout` 计时器。 | `extension.js#5454812` (Cuu)<br>`extension.js#4550122` (StreamIdleTimeoutError)<br>`extension.js#5358771` (STREAM_IDLE_WATCHDOG)<br>`05_empty_keepalive.bin` |
| **§16-8** | `amz-sdk-invocation-id` 在 SDK 重试 attempt 间是否稳定 → 决定幂等键选取 | **100% 绝对稳定**。<br>AWS SDK JS v3 的 retry middleware 实现中，`headers['amz-sdk-invocation-id'] = uuidv4()` 在重试循环的**初始化表达式**中执行一次；随后的每次重试循环体只更新 `headers['amz-sdk-request'] = 'attempt=' + (attempt + 1) + '; max=' + maxAttempts`。<br>• 同一逻辑操作的所有 retry attempts 携带**完全相同的 `amz-sdk-invocation-id`**。<br>• **结论**：网关以 `amz-sdk-invocation-id` 作为全局请求幂等键，完全满足防重发、防双扣要求。 | `extension.js#11068898` (for循环初始化)<br>`extension.js#11072952` (StandardRetryStrategy) |
| **§16-9** | Kiro 用户点"停止"时客户端行为（直接断连？发取消？）→ 决定中断检测方式 | **直接销毁底层 TCP Socket**。<br>Kiro 客户端内部通过 `AbortController` 控制。用户点击 Stop 时，客户端触发 `abort` 事件，直接调用 Node.js 的 `ClientRequest.destroy()`。<br>• **不发送任何额外的 HTTP 取消请求或取消帧**。<br>• 网关侧表现为：HTTP 响应体连接被对端突然关闭（Broken Pipe / ConnectionReset / Hyper body drop）。<br>• **网关中断检测策略**：Rust Axum 网关只需监听客户端流断开（如 `tokio::select!` 监听 axum body channel 挂断），即可立即中止向上游 provider 发起的请求并停止计费结算。 | `extension.js#9271500` (M.destroy())<br>`extension.js#5075458` (AgentStop hook) |

---

## 2. 二进制帧规范 (AWS Event Stream Binary Protocol)

每个 Event-Stream Frame 严格由 4 部分拼接而成，字节序全为大端序 (Big-Endian)：

```
+------------------------------------------------------------------------+
|                            Prelude (12 B)                              |
|  total_length (4B) | headers_length (4B) | prelude_crc32 (4B)          |
+------------------------------------------------------------------------+
|                            Headers (headers_length B)                  |
|  name_len(1B) | name | type(1B=7) | val_len(2B) | value                |
+------------------------------------------------------------------------+
|                            Payload (JSON bytes)                        |
|  {"content": "..."}                                                    |
+------------------------------------------------------------------------+
|                            Message CRC (4 B)                           |
|  message_crc32 (4B) (CRC32 of entire message excluding last 4 bytes)   |
+------------------------------------------------------------------------+
```

### 2.1 基础 Header 集合
每个标准的流式事件帧包含以下 3 个 Header：
1. `:event-type` (type=7, string): 事件名称（如 `assistantResponseEvent`, `toolUseEvent`）
2. `:content-type` (type=7, string): `"application/json"`
3. `:message-type` (type=7, string): `"event"`

异常帧（如错误抛出）包含：
1. `:exception-type` (type=7, string): 异常类名（如 `ValidationException`, `ThrottlingException`）
2. `:content-type` (type=7, string): `"application/json"`
3. `:message-type` (type=7, string): `"exception"`

---

## 3. 消费端事件类型与字段逆向 (`extension.js` 实证)

### 3.1 `assistantResponseEvent`
- **Payload 字段**：
  - `content` (string, 必填): 模型文本增量。
  - `modelId` (string, 可选): 首帧可携带模型 ID（如 `"claude-3-7-sonnet"`）。
- **客户端行为**：
  若 `content` 长度为 0（如 `{"content": ""}`），UI 忽略不输出，但底层重置看门狗（完美用于 Keepalive）。

### 3.2 `toolUseEvent`
- **Payload 字段**：
  - `toolUseId` (string, 必填): 工具调用唯一标识（如 `"tooluse_abc123"`）。
  - `name` (string, 必填): 工具函数名（如 `"fs_read"`, `"execute_bash"`）。
  - `input` (string, 必填): JSON 入参序列化片段（如 `"{\"path\": \"src/main.rs\"}"`）。
  - `stop` (bool, 可选): 最后一个片段设为 `true`，触发 Kiro 执行工具调用。

### 3.3 `reasoningContentEvent`
- **Payload 字段**：
  - `text` (string, 可选): Thinking/Reasoning 思考过程增量文本。
  - `signature` (string, 可选): 思考签名（Anthropic / 深度思考模型）。
  - `redactedContent` (string/bytes, 可选): 加密思考字节。

### 3.4 `contextUsageEvent`
- **Payload 字段**：
  - `contextUsagePercentage` (number, 0.0 ~ 1.0): 当前上下文窗口占用的百分比。
  - **影响**：驱动 Kiro 自动会话压缩阈值。

### 3.5 `metadataEvent`
- **Payload 字段**：
  - `tokenUsage` (object, 可选):
    - `uncachedInputTokens` (number)
    - `outputTokens` (number)
    - `cacheReadInputTokens` (number)
    - `cacheWriteInputTokens` (number)
  - `stopReason` (string, 可选): 如 `"end_turn"`、`"tool_use"`。

---

## 4. 语料文件清单与验证测试结果

在 `docs/p0/stream/` 中产出 6 组二进制验证样本，全部通过 `test_stream_corpus.js` 测试：

| 文件名 | 事件类型 | 验证项 | 状态 |
|---|---|---|---|
| `01_assistant_response.bin` | `assistantResponseEvent` | 3 个分片文本流，首帧带 `modelId`，组装文本无缝拼接 | **PASS** |
| `02_tool_use.bin` | `toolUseEvent` | 3 个分片入参流，分片拼接为合法 JSON，末尾 `stop: true` | **PASS** |
| `03_reasoning_content.bin` | `reasoningContentEvent` | 思考链明文流 + 思考签名 (`signature`) 完整解析 | **PASS** |
| `04_context_and_metadata.bin` | `contextUsageEvent` + `metadataEvent` | 4 种 token 用量 + 结束原因 (`end_turn`) + 上下文百分比 | **PASS** |
| `05_empty_keepalive.bin` | `assistantResponseEvent` (空) | 空 content 帧验证：不引起 UI 错乱，有效喂养看门狗 | **PASS** |
| `06_exception_frame.bin` | `ValidationException` | 异常帧正确触发客户端错误处理体系 | **PASS** |

所有帧均通过了 **Prelude CRC32** 与 **Message CRC32** 的双重校验，且经 Kiro 真实 `ndi()` 解码器与 `Iuu()` 映射器完整解码验证。
