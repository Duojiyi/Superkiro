# Phase 0 (P0) 阶段门双盲审计记录 (PHASE-GATE.md)

> **审计日期**：2026-09-10  
> **审计对象**：Phase 0 (阻塞决策与技术探针) 全部任务交付物、测试语料及 Spec §16 决议。  
> **基线 Spec**：`docs/superpowers/specs/2026-09-09-kiro-byok-design.md` (v0.4)  
> **执行标准**：TODO §0.3 阶段门协议。

---

## 1. 阶段目标与交付物核验

Phase 0 核心目标：通过真实 Kiro 1.0.437 / Agent 1.0.794 逆向分析与探针实测，全面穿透并终结 Spec §16 所列全部 11 项未知数，形成不依赖任何主观臆测的技术事实基线，为 Phase P1（Rust 网关核心）提供确凿的架构依据与真实二进制测试语料。

### 1.1 任务矩阵核对
- [x] **G0-1**：Spec v0.4 架构基线复核与确认 (`docs/audits/p0/G0-1.md`)
- [x] **P0-1**：Stub 抓包测试服务器 (`spikes/p0-stub/server.py`, `docs/audits/p0/P0-1.md`)
- [x] **P0-2**：Env 改道覆盖面实测分析 (`docs/p0/coverage.md`, `docs/audits/p0/P0-2.md`)
- [x] **P0-3**：接管持久性决策报告 (`docs/p0/persistence.md`, `docs/audits/p0/P0-3.md`)
- [x] **P0-4**：真实认证与 Token 存储逆向分析 (`docs/p0/auth.md`, `docs/audits/p0/P0-4.md`)
- [x] **P0-5**：管理面响应 Schema 与最小必填集 (`docs/p0/mgmt-schema.md`, `docs/audits/p0/P0-5.md`)
- [x] **P0-6**：对话 Event-Stream 逆向与二进制语料 (`docs/p0/stream.md`, `docs/audits/p0/P0-6.md`)
- [x] **P0-7**：辅助调用面控制与运行态检测 (`docs/p0/aux-surfaces.md`, `docs/audits/p0/P0-7.md`)
- [x] **P0-8**：Spec §16 决策汇总与 Spec 待定标签清理 (`docs/p0/DECISIONS.md`, `docs/audits/p0/P0-8.md`)

---

## 2. 审计 A（Spec 覆盖度盲审）

**审计原则**：不看实现声明与过程代码，仅对照 Spec 对应章节的要求，逐一确认能否指认出该要求对应的交付物与测试依据。

| Spec 要求项 | 对应 P0 交付物 | 证据与验收判定 | 判定 |
|---|---|---|---|
| **§16-1** env 改道覆盖面 | `docs/p0/coverage.md` | 证实 `acp-q-client` 固定写死 endpoint，必须配合扩展轻量补丁，纯 env 存在漏网；且扩展不在 `product.json` checksums 内。 | **PASS** |
| **§16-2** 接管持久性方案 | `docs/p0/persistence.md` | 拍板放弃快捷方式劫持，采用 VSCode `settings.json` + `extension.js` 精准桩点打标。 | **PASS** |
| **§16-3, §16-5** 登录态与 Token 验签 | `docs/p0/auth.md`, `samples/auth_token_sample.json` | 锁定 `~/.aws/sso/cache/kiro-auth-token.json` 格式与 `fs.watchFile` 热重载特性；证实 Kiro 零验签，网关可自主签发 JWT。 | **PASS** |
| **§16-6** 管理面响应 Schema | `docs/p0/mgmt-schema.md`, `samples/mgmt_*.json` | 锁定 4 个管理端点 schema 与最小必填字段，验证了模型与套餐虚拟化的可行性。 | **PASS** |
| **§16-4** Event-Stream 二进制结构 | `docs/p0/stream.md`, `docs/p0/stream/*.bin` | 标准 AWS Event-Stream 格式实证，分片 JSON 与 `stop: true` 工具帧机制锁定，6 组二进制测试帧全部就绪。 | **PASS** |
| **§16-7** 看门狗阈值与保活 | `docs/p0/stream.md` | 锁定 60s 预警 / 300s 断流阈值，确定每 20~25s 发送空 `assistantResponseEvent` 保活策略。 | **PASS** |
| **§16-8** `invocation-id` 稳定性 | `docs/p0/stream.md` | 锁定 AWS SDK 在循环外初始化 UUIDv4，全 attempt 稳定不变，确立为全局幂等键。 | **PASS** |
| **§16-9** 客户端点停止行为 | `docs/p0/stream.md` | 锁定客户端直接调用 `ClientRequest.destroy()` 掐断 TCP 连接，确立网关监听断连即中止上游策略。 | **PASS** |
| **§16-10** autocomplete 禁用与降级 | `docs/p0/aux-surfaces.md` | 锁定 `settings.json` 禁用与 HTTP 429 `MONTHLY_REQUEST_COUNT` 官方原生错误降级。 | **PASS** |
| **§16-11** Kiro 运行态可靠检测 | `docs/p0/aux-surfaces.md` | 锁定 Windows 原生命名互斥量 `OpenMutexW(SYNCHRONIZE, FALSE, L"kiro")` 实测 handle=384 与跨平台进程遍历。 | **PASS** |
| **Spec 全文待定标签清理** | `superpowers/specs/2026-09-09-kiro-byok-design.md` | 全文 0 处遗留 `【待P0验证】`，所有技术未知数已全部转为实证结论。 | **PASS** |

**审计 A 结论**：Spec §16 全部要求 100% 覆盖，每一条均能明确指出对应的交付文档、测试脚本与源码级反汇编实证。**PASS**。

---

## 3. 审计 B（对抗破坏性盲审）

**审计原则**：假设系统存在脆弱性、假阳性或边界缺陷，端到端执行测试并尝试反例输入进行破坏。

### 3.1 端到端自动化测试执行
1. **Stub 核心数据流测试 (`spikes/p0-stub/test_stub.py`)**：
   - 验证点：包含 `\0` 空字节二进制报文、2MB 大流量报文。
   - 结果：完整通过（`PASS: Null bytes preserved perfectly`, `PASS: 2MB payload captured with zero truncation`）。
2. **Event-Stream 二进制编解码测试 (`spikes/p0-stub/test_stream_corpus.js`)**：
   - 验证点：调用 Kiro 官方解码器 `ndi()` 与映射器 `Iuu()` 解析 6 组二进制帧。
   - 结果：全部通过（文本流、工具分片组装、思考链签名、用量快照、空保活帧、异常帧全部成功反序列化且校验 CRC32）。
3. **样本合规性与脱敏测试**：
   - 验证点：遍历 `docs/p0/samples/` 全部 JSON 样本，确保均为有效 JSON 且无任何泄漏密钥（无 `AKIA`、无 Google 真实 Token）。
   - 结果：全部通过。

### 3.2 对抗性破坏实验与防御验证
- **破坏实验 1：帧 CRC32 单 bit 翻转篡改**
  - 构造方式：对 `01_assistant_response.bin` 载荷中段进行异或翻转。
  - 实验表现：CRC32 校验函数精确捕获异常（Expected != Actual），立即阻止损坏帧进入下游。
- **破坏实验 2：空保活帧注入对 UI 的污染测试**
  - 构造方式：向解码管线注入 `{"content": ""}` 的 `assistantResponseEvent`。
  - 实验表现：`Iuu()` 过滤条件 `n.content.length > 0` 成功拦截，未产生任何无效 Token 或 UI 空格，但重置了看门狗时钟。
- **破坏实验 3：互斥量状态误判测试**
  - 构造方式：探测非真实存在的互斥量名称（如 `Kiro_Fake_Mutex`）。
  - 实验表现：Win32 API 精确返回 `None` 且错误码为 `ERROR_FILE_NOT_FOUND (2)`，无任何进程挂起或异常崩溃。

**审计 B 结论**：端到端用例与对抗用例全部通过，未发现边界失效、数据损毁或假阳性。**PASS**。

---

## 4. 阶段门裁决 (Phase Gate Decision)

- **审计 A**：PASS
- **审计 B**：PASS
- **裁决结果**：**PHASE 0 阶段门通过 (APPROVED)**。
- **准入许可**：**正式批准进入 Phase P1（网关核心：最小可用闭环）阶段的实施**。
