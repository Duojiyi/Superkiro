export interface TimeoutProfile {
  headers_secs: number; attempt_secs: number; total_secs: number;
  commit_secs: number; started_secs: number; idle_secs: number;
}
export interface RuntimeSettings {
  standard: TimeoutProfile; reasoning: TimeoutProfile; claude: TimeoutProfile;
  openai_reasoning_idle_secs: number; keepalive_secs: number;
}
export interface RuntimeConfig {
  revision: string; settings: RuntimeSettings;
  audit: Array<{revision: string; previous_revision: string; reason: string; created_at_secs: number}>;
}
export interface RuntimeUpdate {expected_revision: string; reason: string; settings: RuntimeSettings}
export const timeoutFields: Array<{key: keyof TimeoutProfile; label: string; min: number; max: number}> = [
  {key: 'headers_secs', label: '上游响应头等待', min: 1, max: 300},
  {key: 'attempt_secs', label: '单次启动等待', min: 1, max: 900},
  {key: 'total_secs', label: '重试启动总预算', min: 1, max: 1800},
  {key: 'commit_secs', label: '客户端保活启动', min: 1, max: 45},
  {key: 'started_secs', label: '单次请求时长上限', min: 60, max: 3600},
  {key: 'idle_secs', label: '流空闲等待', min: 10, max: 3600},
];
export function runtimeError(s: RuntimeSettings): string {
  if (!s || typeof s !== 'object') return '服务器未返回有效运行配置';
  for (const p of [s.standard, s.reasoning, s.claude]) {
    if (!p || typeof p !== 'object') return '服务器未返回完整超时配置';
    if (timeoutFields.some(f => !Number.isInteger(p[f.key]) || p[f.key] < f.min || p[f.key] > f.max)) return '所有超时须为范围内的整数秒';
    if (p.headers_secs > p.attempt_secs || p.attempt_secs > p.total_secs || p.total_secs > p.started_secs || p.idle_secs > p.started_secs || s.keepalive_secs >= p.idle_secs || p.commit_secs > p.total_secs) return '须满足：响应头 ≤ 单次启动 ≤ 启动总预算 ≤ 请求上限，空闲 ≤ 请求上限，保活间隔 < 空闲';
  }
  if (!Number.isInteger(s.keepalive_secs) || s.keepalive_secs < 1 || s.keepalive_secs > 25) return '保活间隔须为 1–25 秒';
  if (!Number.isInteger(s.openai_reasoning_idle_secs) || s.openai_reasoning_idle_secs < 10 || s.openai_reasoning_idle_secs > 3600 || s.openai_reasoning_idle_secs > s.reasoning.started_secs) return 'OpenAI 思考空闲须为 10–3600 秒且不超过思考请求上限';
  return '';
}

export function runtimeReasonError(reason: string): string {
  return !reason.trim() || new TextEncoder().encode(reason.trim()).length > 1024 || /[\u0000-\u001f\u007f-\u009f]/.test(reason)
    ? '请填写修改原因：最多 1024 UTF-8 字节，不可含控制字符' : '';
}
