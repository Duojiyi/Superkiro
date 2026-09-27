// One vocabulary and one colour for every status the console shows.
import type {AdminCardItem, AdminAnnouncement} from './api';

export type Tone = 'success' | 'warning' | 'danger' | 'neutral' | 'info' | 'outline';
export interface StatusView {label: string; tone: Tone; title?: string}

const CARD: Record<AdminCardItem['status'], StatusView> = {
  unactivated: {label: '未激活', tone: 'neutral'},
  active: {label: '使用中', tone: 'success'},
  frozen: {label: '已冻结', tone: 'warning'},
  banned: {label: '已封禁', tone: 'danger'},
  expired: {label: '已到期', tone: 'outline'},
  voided: {label: '已作废', tone: 'neutral'},
};

export function cardStatusView(status: string): StatusView {
  return CARD[status as AdminCardItem['status']] ?? {label: status || '未知', tone: 'outline'};
}

/**
 * A card's status as it works now. The server checks a card's validity only when the card is
 * used, so one past its date still reads active (or frozen): it is 已到期. The server's own
 * effectiveStatus comes first when it sends one.
 */
export function cardState(card: {status: AdminCardItem['status']; validUntil?: number | null; effectiveStatus?: unknown}, nowSecs: number): AdminCardItem['status'] {
  if (typeof card.effectiveStatus === 'string' && card.effectiveStatus in CARD) return card.effectiveStatus as AdminCardItem['status'];
  return (card.status === 'active' || card.status === 'frozen') && card.validUntil != null && card.validUntil <= nowSecs ? 'expired' : card.status;
}

type Row = Record<string, unknown>;

/** Seconds of cooldown left for a key, or 0. */
export const keyCooldownLeft = (key: Row, nowSecs: number) => Math.max(0, Number(key.cooldown_until ?? 0) - nowSecs);

/** A cooldown's length in words: minutes, then hours, then days. */
export function cooldownText(secs: number): string {
  const minutes = Math.max(1, Math.ceil(secs / 60));
  return minutes < 90 ? `${minutes} 分钟` : minutes < 48 * 60 ? `约 ${Math.round(minutes / 60)} 小时` : `约 ${Math.round(minutes / 1440)} 天`;
}

/**
 * Why a Key needs attention now, from its live health: cooling down (until a time, or for as long
 * as the server says so), degraded (its cooldown is over and it takes requests again, but none has
 * succeeded yet) or unhealthy (refused as invalid: out until reset). A disabled Key needs none.
 */
export function keyAlert(key: Row, nowSecs: number): 'cooldown' | 'degraded' | 'unhealthy' | null {
  if (key.enabled === false) return null;
  if (key.health_state === 'unhealthy') return 'unhealthy';
  if (key.health_state === 'degraded') return 'degraded';
  const until = Number(key.cooldown_until ?? 0);
  return keyCooldownLeft(key, nowSecs) > 0 || (key.health_state === 'cooldown' && !(until > 0)) ? 'cooldown' : null;
}

const FAILURE: Record<string, string> = {
  upstream_service: '上游服务报错',
  protocol: '上游回复无法解析',
  timeout: '超时',
  empty: '上游返回空内容',
  transport: '网络连接失败',
};
const HTTP_FAILURE: Record<number, string> = {401: 'Key 无效或被拒绝', 403: '没有权限', 404: '上游没有这个模型或地址', 429: '限流', 529: '上游过载'};

/**
 * A failure as the server names it, in attempt traces and as a Key's last error (http_429,
 * timeout, transport, …), in words. Anything else, such as older free text, is shown as it is.
 */
export function failureLabel(value: unknown): string {
  if (typeof value !== 'string' || !value) return '';
  const http = /^http_(\d{3})$/.exec(value);
  if (!http) return FAILURE[value] ?? value;
  const code = Number(http[1]), words = HTTP_FAILURE[code] ?? (code >= 500 ? '上游服务出错' : '');
  return words ? `HTTP ${code} · ${words}` : `HTTP ${code}`;
}

/** The upstream refused the Key itself (invalid, HTTP 401, or without permission, 403): only a new secret fixes it. */
export const credentialFailure = (key: Row) => key.health_state === 'unhealthy' || ['http_401', 'http_403'].includes(String(key.last_error ?? ''));

export function keyStatusView(key: Row, nowSecs: number): StatusView {
  if (key.enabled === false) return {label: '已停用', tone: 'neutral'};
  const lastError = failureLabel(key.last_error), error = lastError ? `最近错误：${lastError}` : undefined;
  const alert = keyAlert(key, nowSecs), cooldown = keyCooldownLeft(key, nowSecs);
  if (alert === 'unhealthy') return {label: '不可用', tone: 'danger', title: error};
  if (alert === 'degraded') return {label: '冷却后试用中', tone: 'warning', title: ['冷却已结束，重新接请求，还没成功过', error].filter(Boolean).join('\n')};
  if (alert === 'cooldown') {
    if (cooldown <= 0) return {label: '冷却中', tone: 'warning', title: error};
    return {label: `冷却中 · ${cooldownText(cooldown)}`, tone: 'warning', title: [`${cooldownText(cooldown)}后恢复`, error].filter(Boolean).join('\n')};
  }
  if (key.health_state === 'healthy' || key.health_state === 'cooldown') return {label: '正常', tone: 'success'};
  return {label: '未检测', tone: 'outline'};
}

/** A provider's API format as shown: the server's `format` (open_ai, anthropic), or `api_type` in older data. */
export function providerFormatLabel(provider: Row): string | null {
  const format = typeof provider.format === 'string' ? provider.format : typeof provider.api_type === 'string' ? provider.api_type : '';
  return !format ? null : ['open_ai', 'openai'].includes(format) ? 'OpenAI' : format === 'anthropic' ? 'Anthropic' : format;
}

/**
 * The saved billing state against its ceiling, where every save fails and so every request is
 * refused: archive the ledger soon from the server's warning level, and now from the level at
 * which the server logs "archive … now" (a quarter of the ceiling: billing's STATE_URGENT_BYTES).
 * Null when the server does not report its size.
 */
export function storageLevel(stats: {stateBytes?: unknown; stateWarningBytes?: unknown; stateCeilingBytes?: unknown} | null | undefined):
  {bytes: number; warning: number; urgent: number; ceiling: number; level: 'ok' | 'soon' | 'now'} | null {
  const [bytes, warning, ceiling] = [stats?.stateBytes, stats?.stateWarningBytes, stats?.stateCeilingBytes].map(Number);
  if (![bytes, warning, ceiling].every(value => Number.isFinite(value) && value >= 0) || !ceiling) return null;
  const urgent = ceiling / 4;
  return {bytes, warning, urgent, ceiling, level: bytes >= urgent ? 'now' : bytes >= warning ? 'soon' : 'ok'};
}

// Why the server could not save, in its words (admin.rs persistence_problem), and what fixes it.
const PERSISTENCE_PROBLEMS: Record<string, {text: string; fix: string}> = {
  'The saved state has reached its size ceiling; archive old ledger entries': {text: '保存的数据到了大小上限', fix: '请现在归档旧账本（下方“归档账本…”）'},
  'The disk holding the saved state is full': {text: '保存数据的磁盘满了', fix: '请清理服务器上保存数据的磁盘'},
  'The saved state cannot be written: permission denied': {text: '没有写入保存文件的权限', fix: '请检查服务器进程对保存文件所在目录的写权限'},
  'Saving this state needs the master key, which is not configured': {text: '保存这些数据需要主密钥，服务器没有配置', fix: '请在服务器上配置主密钥后重启'},
  'The saved state could not be written': {text: '保存的数据写不进去', fix: '请查看服务器日志'},
};

/**
 * Whether the server's latest change is saved, and when it last saved. While a save has failed the
 * server refuses every change and every request, so a problem is the most urgent thing to fix.
 * Null when the server reports neither.
 */
export function persistence(stats: {persistenceReady?: unknown; persistenceError?: unknown; lastSavedAtSecs?: unknown} | null | undefined):
  {ready: boolean | null; problem: {text: string; fix: string} | null; savedAt: number | null} | null {
  const ready = typeof stats?.persistenceReady === 'boolean' ? stats.persistenceReady : null;
  const savedAt = typeof stats?.lastSavedAtSecs === 'number' && stats.lastSavedAtSecs > 0 ? stats.lastSavedAtSecs : null;
  if (ready === null && savedAt === null) return null;
  const said = typeof stats?.persistenceError === 'string' ? stats.persistenceError : '';
  const problem = ready === false ? PERSISTENCE_PROBLEMS[said] ?? {text: said || '保存失败', fix: '请查看服务器日志'} : null;
  return {ready, problem, savedAt};
}

export const TRACE_IN_PROGRESS = ['pending', 'running', 'in_progress'];

/** 进行中 for longer than this: no request runs that long, so it was most likely cut off (a restart, a lost connection). */
export const TRACE_STUCK_SECS = 30 * 60;

/** A request still 进行中 more than 30 minutes after it started. */
export function traceStuck(trace: {status?: unknown; ts?: unknown}, nowSecs: number): boolean {
  const started = Number(trace.ts);
  return TRACE_IN_PROGRESS.includes(String(trace.status)) && Number.isFinite(started) && started > 0 && nowSecs - started > TRACE_STUCK_SECS;
}

export function traceStatusView(status: unknown, stuck = false): StatusView {
  switch (String(status)) {
    case 'success': return {label: '成功', tone: 'success'};
    case 'error': return {label: '失败', tone: 'danger'};
    case 'client_aborted': return {label: '客户端中断', tone: 'warning'};
    default: return TRACE_IN_PROGRESS.includes(String(status))
      ? (stuck ? {label: '可能已中断', tone: 'warning'} : {label: '进行中', tone: 'info'})
      : {label: String(status ?? '未知'), tone: 'outline'};
  }
}

const ERROR_CLASS: Record<string, string> = {
  upstream_start_failed: '上游未响应（未开始输出）',
  stream_incomplete: '输出中断',
  empty_completion: '上游返回空内容',
  settlement_failed: '结算失败',
  // Refused before any upstream was tried.
  no_price: '没有生效中的价格',
  model_not_listed: '模型未上架',
  model_retired: '模型已下架',
  invalid_model: '模型 ID 无效',
  no_route: '无可用线路',
  unsupported_capability: '模型不支持这项能力',
  // Refused for the card's own balance or limits, before any upstream was tried.
  insufficient_balance: '余额不足',
  concurrency_limit: '超过并发上限',
  usage_limit: '超过每日或每月用量上限',
};

/** A failure class in words; unknown classes are shown as they are. */
export const errorClassLabel = (value: unknown): string =>
  typeof value === 'string' && value ? ERROR_CLASS[value] ?? value : '';

/** Refusals for the card's own balance or limits: they say nothing about the model or its route. */
export const CARD_LIMIT_REFUSALS = ['insufficient_balance', 'concurrency_limit', 'usage_limit'];

const points = (micro: unknown) => typeof micro === 'number' && Number.isFinite(micro)
  ? (micro / 1_000_000).toLocaleString('en-US', {maximumFractionDigits: 2}) : null;

/** A request's failure in words, with what a balance refusal needed: 余额不足：需要 20.3 积分，余额 15 积分. */
export function traceFailureText(trace: {error_class?: string | null; needed_micro_credits?: unknown; available_micro_credits?: unknown}): string {
  const label = errorClassLabel(trace.error_class);
  if (trace.error_class !== 'insufficient_balance') return label;
  const needed = points(trace.needed_micro_credits), available = points(trace.available_micro_credits);
  return needed === null ? label : `${label}：需要 ${needed} 积分${available === null ? '' : `，余额 ${available} 积分`}`;
}

/** What a refusal for the card's own limits means for the customer, for the request's details. */
export const CARD_LIMIT_NOTE: Record<string, string> = {
  insufficient_balance: '请求开始前要按最大输出预留积分；余额不够预留，这次没有发给上游，也没有扣费。',
  concurrency_limit: '这张卡同时进行的请求已到上限，这次没有发给上游，也没有扣费。',
  usage_limit: '这张卡已到每日或每月用量上限，这次没有发给上游，也没有扣费。',
};

export function noticeStatusView(notice: AdminAnnouncement, nowSecs: number): StatusView {
  if (!notice.enabled) return {label: '已撤回', tone: 'neutral'};
  if (notice.expires_at && notice.expires_at < nowSecs) return {label: '已到期', tone: 'outline'};
  return {label: '生效中', tone: 'success'};
}

export const NOTICE_LEVEL: Record<AdminAnnouncement['level'], StatusView> = {
  info: {label: '普通', tone: 'info'},
  warning: {label: '预警', tone: 'warning'},
  critical: {label: '紧急', tone: 'danger'},
};

export function stopReasonView(reason: unknown): StatusView | null {
  if (typeof reason !== 'string' || !reason) return null;
  if (reason === 'end_turn' || reason === 'stop_sequence') return {label: '正常结束', tone: 'success', title: reason};
  if (reason === 'tool_use') return {label: '调用工具', tone: 'info', title: reason};
  if (reason === 'max_tokens') return {label: '达到输出上限', tone: 'warning', title: reason};
  return {label: reason, tone: 'outline'};
}

/** A model as customers meet it: 在售 (listed), 隐藏 (not listed, still served to whoever uses its ID), 已下架 (refused). */
export function modelStateView(model: Row): StatusView {
  if (model.retired === true) return {label: '已下架', tone: 'neutral', title: '不在客户的模型列表里，请求会被拒绝'};
  if (model.visible === false) return {label: '隐藏', tone: 'outline', title: '不在客户的模型列表里；已经在用这个模型 ID 的客户仍可调用'};
  return {label: '在售', tone: 'success'};
}

/** A 测试 result in a few words: 成功 · 首字 410 ms, or what failed. */
export function probeView(result: {ok?: unknown; status?: unknown; ttft_ms?: unknown; latency_ms?: unknown; error?: unknown; reply?: unknown}): StatusView {
  const ms = (value: unknown) => `${Math.round(Number(value))} ms`;
  if (result.ok === true) return {label: typeof result.ttft_ms === 'number' ? `成功 · 首字 ${ms(result.ttft_ms)}` : `成功 · 耗时 ${ms(result.latency_ms)}`, tone: 'success',
    title: typeof result.reply === 'string' && result.reply ? `回复：${result.reply}` : undefined};
  const error = typeof result.error === 'string' && result.error ? result.error : typeof result.status === 'number' && result.status ? `HTTP ${result.status}` : '没有回复';
  return {label: `失败：${error}`, tone: 'danger', title: error};
}

/** Price versions of one model and rate card: the one in force, the scheduled ones, the replaced ones. */
export function priceVersionView(version: Row, versions: Row[], nowSecs: number): StatusView {
  const from = Number(version.effective_from_secs);
  if (!Number.isFinite(from)) return {label: '未知', tone: 'outline'};
  if (from > nowSecs) return {label: '已排期', tone: 'info'};
  const newer = versions.some(other => other !== version && other.model === version.model && other.rate_card_id === version.rate_card_id
    && Number(other.effective_from_secs) <= nowSecs && Number(other.effective_from_secs) > from);
  return newer ? {label: '已被替代', tone: 'neutral'} : {label: '生效中', tone: 'success'};
}
