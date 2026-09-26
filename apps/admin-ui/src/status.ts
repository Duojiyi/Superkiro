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

type Row = Record<string, unknown>;

/** Seconds of cooldown left for a key, or 0. */
export const keyCooldownLeft = (key: Row, nowSecs: number) => Math.max(0, Number(key.cooldown_until ?? 0) - nowSecs);

export function keyStatusView(key: Row, nowSecs: number): StatusView {
  if (key.enabled === false) return {label: '已停用', tone: 'neutral'};
  const cooldown = keyCooldownLeft(key, nowSecs);
  if (key.health_state === 'cooldown' || cooldown > 0) {
    if (cooldown <= 0) return {label: '冷却中', tone: 'warning'};
    const minutes = Math.max(1, Math.ceil(cooldown / 60));
    return {label: `冷却中 · ${minutes} 分钟`, tone: 'warning', title: `约 ${minutes} 分钟后恢复`};
  }
  if (key.health_state === 'degraded') return {label: '异常', tone: 'danger'};
  if (key.health_state === 'healthy') return {label: '正常', tone: 'success'};
  return {label: '未检测', tone: 'outline'};
}

export const TRACE_IN_PROGRESS = ['pending', 'running', 'in_progress'];

export function traceStatusView(status: unknown): StatusView {
  switch (String(status)) {
    case 'success': return {label: '成功', tone: 'success'};
    case 'error': return {label: '失败', tone: 'danger'};
    case 'client_aborted': return {label: '客户端中断', tone: 'warning'};
    default: return TRACE_IN_PROGRESS.includes(String(status)) ? {label: '进行中', tone: 'info'} : {label: String(status ?? '未知'), tone: 'outline'};
  }
}

const ERROR_CLASS: Record<string, string> = {
  upstream_start_failed: '上游未响应（未开始输出）',
  stream_incomplete: '输出中断',
  empty_completion: '上游返回空内容',
  settlement_failed: '结算失败',
};

/** A failure class in words; unknown classes are shown as they are. */
export const errorClassLabel = (value: unknown): string =>
  typeof value === 'string' && value ? ERROR_CLASS[value] ?? value : '';

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

/** A 测试 result in a few words: 成功 · 首字 410 ms, or what failed. */
export function probeView(result: {ok?: unknown; status?: unknown; ttft_ms?: unknown; latency_ms?: unknown; error?: unknown}): StatusView {
  const ms = (value: unknown) => `${Math.round(Number(value))} ms`;
  if (result.ok === true) return {label: typeof result.ttft_ms === 'number' ? `成功 · 首字 ${ms(result.ttft_ms)}` : `成功 · 耗时 ${ms(result.latency_ms)}`, tone: 'success'};
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
