// How numbers, money, tokens, times and IDs are shown across the console.
// Pure functions without imports, so the Node tests can load this file directly.

const pad = (value: number) => String(value).padStart(2, '0');
const isNumber = (value: unknown): value is number => typeof value === 'number' && Number.isFinite(value);

function toDate(secs: unknown): Date | null {
  if (!isNumber(secs) || secs <= 0) return null;
  const date = new Date(secs * 1000);
  return Number.isFinite(date.getTime()) ? date : null;
}

const sameDay = (a: Date, b: Date) =>
  a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();

/** Credit balances and totals: thousands separators, at most two decimals. 2,000 · 75.93 */
export function formatCredits(points: unknown): string {
  if (!isNumber(points)) return '—';
  return points.toLocaleString('en-US', {maximumFractionDigits: 2});
}

/** Credits stored as micro-credits (1 credit = 1,000,000). */
export const formatCreditsMicro = (micro: unknown): string =>
  isNumber(micro) ? formatCredits(micro / 1_000_000) : '—';

/** One request's charge: four decimals; nothing charged is a dash. */
export function formatCharge(micro: unknown): string {
  if (!isNumber(micro) || micro === 0) return '—';
  return (micro / 1_000_000).toFixed(4);
}

/** Money from micro-CNY: ¥ with two decimals; tiny non-zero amounts keep four. */
export function formatMoney(microCny: unknown): string {
  if (!isNumber(microCny)) return '—';
  const yuan = microCny / 1_000_000;
  const magnitude = Math.abs(yuan);
  const digits = magnitude > 0 && magnitude < 0.01 ? 4 : 2;
  const text = magnitude.toLocaleString('en-US', {minimumFractionDigits: digits, maximumFractionDigits: digits});
  return `${yuan < 0 ? '-' : ''}¥${text}`;
}

/** The exact amount, for tooltips next to a rounded one. */
export function formatMoneyExact(microCny: unknown): string {
  if (!isNumber(microCny)) return '';
  return `¥${(microCny / 1_000_000).toLocaleString('en-US', {maximumFractionDigits: 6})}`;
}

/** Whole counts with thousands separators. */
export const formatCount = (value: unknown): string =>
  isNumber(value) ? Math.round(value).toLocaleString('en-US') : '—';

const oneDecimal = (value: number) => value.toFixed(1).replace(/\.0$/, '');

/** Token counts: 820 · 36.8K · 1.2M. The exact count belongs in a tooltip (formatCount). */
export function formatTokenCount(value: unknown): string {
  if (!isNumber(value)) return '—';
  const magnitude = Math.abs(value);
  if (magnitude >= 1_000_000) return `${oneDecimal(value / 1_000_000)}M`;
  if (magnitude >= 1_000) return `${oneDecimal(value / 1_000)}K`;
  return String(Math.round(value));
}

/** Durations: under a second in ms, otherwise seconds with one decimal. 820 ms · 1.2 s */
export function formatDuration(ms: unknown): string {
  if (!isNumber(ms) || ms < 0) return '—';
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(1)} s`;
}

/** Output speed as whole tokens per second. */
export const formatSpeed = (tokensPerSecond: unknown): string =>
  isNumber(tokensPerSecond) && tokensPerSecond > 0 ? `${Math.round(tokensPerSecond)} tok/s` : '—';

export const formatPercent = (value: unknown, digits = 1): string =>
  isNumber(value) ? `${value.toFixed(digits)}%` : '—';

/** YYYY-MM-DD HH:mm:ss, local time. */
export function formatFullDateTime(secs: unknown): string {
  const date = toDate(secs);
  if (!date) return '—';
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

/** Today's times as HH:mm:ss; other days as YYYY-MM-DD HH:mm. */
export function formatDateTime(secs: unknown, now = Date.now()): string {
  const date = toDate(secs);
  if (!date) return '—';
  if (sameDay(date, new Date(now))) return `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** Dense lists (traces): today HH:mm:ss, otherwise MM-DD HH:mm. */
export function formatListTime(secs: unknown, now = Date.now()): string {
  const date = toDate(secs);
  if (!date) return '—';
  if (sameDay(date, new Date(now))) return `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
  return `${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** MM-DD */
export function formatShortDate(secs: unknown): string {
  const date = toDate(secs);
  return date ? `${pad(date.getMonth() + 1)}-${pad(date.getDate())}` : '—';
}

/** HH:mm */
export function formatClock(secs: unknown): string {
  const date = toDate(secs);
  return date ? `${pad(date.getHours())}:${pad(date.getMinutes())}` : '—';
}

/** 刚刚 · 3 分钟前 · 2 小时前 · 5 天前 (and 后 for future times). */
export function formatRelative(secs: unknown, now = Date.now()): string {
  if (!isNumber(secs)) return '—';
  const diff = Math.round(now / 1000 - secs);
  const future = diff < 0;
  const seconds = Math.abs(diff);
  if (seconds < 45) return future ? '即将' : '刚刚';
  const unit = seconds < 3600 ? `${Math.max(1, Math.floor(seconds / 60))} 分钟`
    : seconds < 86400 ? `${Math.floor(seconds / 3600)} 小时`
    : `${Math.floor(seconds / 86400)} 天`;
  return future ? `${unit}后` : `${unit}前`;
}

export type RemainingTone = 'normal' | 'warning' | 'danger';

/** Time left until an expiry: 剩 24 天; amber within 7 days, red once passed. */
export function formatRemaining(secs: unknown, now = Date.now()): {text: string; tone: RemainingTone} {
  if (!isNumber(secs)) return {text: '—', tone: 'normal'};
  const left = secs - now / 1000;
  if (left <= 0) return {text: '已过期', tone: 'danger'};
  if (left < 86400) return {text: `剩 ${Math.max(1, Math.ceil(left / 3600))} 小时`, tone: 'warning'};
  const days = Math.floor(left / 86400);
  return {text: `剩 ${days} 天`, tone: left <= 7 * 86400 ? 'warning' : 'normal'};
}

/** Minutes left in a session: 剩余 12 分钟, or m:ss in the last two minutes. */
export function formatSessionLeft(ms: number): string {
  if (!isNumber(ms) || ms <= 0) return '已到期';
  if (ms < 120_000) {
    const seconds = Math.ceil(ms / 1000);
    return `${Math.floor(seconds / 60)}:${pad(seconds % 60)}`;
  }
  return `${Math.ceil(ms / 60_000)} 分钟`;
}

const BATCH_REFERENCE = /批次 (\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z) [0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}(?:-#(\d+))?/gi;

/** Batch references in notes, read as local time: 批次 09-20 23:02 · #1. The raw note goes in the tooltip. */
export function formatBatchNote(note: unknown): string {
  if (typeof note !== 'string') return '';
  return note.replace(BATCH_REFERENCE, (match, iso: string, index?: string) => {
    const date = new Date(iso);
    if (!Number.isFinite(date.getTime())) return match;
    const when = `${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
    return `批次 ${when}${index ? ` · #${index}` : ''}`;
  });
}

/** The first characters of a revision hash, enough to tell versions apart. */
export const shortHash = (value: unknown, length = 7): string =>
  typeof value === 'string' && value ? value.slice(0, length) : '—';

export type IdKind = 'card' | 'trace' | 'device' | 'hash' | 'generic';

/**
 * Middle-truncated IDs: card-3184…d133 keeps the prefix and the last four; traces keep the
 * first eight and the last six. Short IDs are shown whole.
 */
export function shortId(value: unknown, kind: IdKind = 'generic'): string {
  const text = typeof value === 'string' ? value : value == null ? '' : String(value);
  if (kind === 'hash') return text.slice(0, 7);
  if (kind === 'card') {
    const match = /^(card-)(.+)$/.exec(text);
    if (match && text.length > 15) return `${match[1]}${match[2].slice(0, 4)}…${text.slice(-4)}`;
  }
  if (kind === 'device') {
    const match = /^([A-Za-z]+_)(.+)$/.exec(text);
    if (match && text.length > 14) return `${match[1]}${match[2].slice(0, 4)}…${text.slice(-4)}`;
  }
  const head = 8, tail = kind === 'trace' ? 6 : 4;
  return text.length > head + tail + 4 ? `${text.slice(0, head)}…${text.slice(-tail)}` : text;
}
