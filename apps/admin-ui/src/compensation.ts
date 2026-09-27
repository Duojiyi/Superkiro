// 补偿这次扣费: an adjustment that gives back what one request (or a few) charged, with a reason
// that says which, linked to the request so the card's history can lead back to it, and what the
// server found when it refuses one (crates/billing/src/engine.rs check_compensation). Pure
// functions, so the rules can be tested without a browser.
import {formatCredits, formatListTime, shortId} from './format';
import {errorClassLabel, traceStatusView} from './status';

/** The trace fields a compensation reads. */
export interface ChargedRequest {
  ts: number;
  invocation_id?: string;
  exposed_model?: string;
  status?: string;
  error_class?: string | null;
  credits_charged?: number;
}

/** What 调账 opens with: the points to give back, the reason, and the request it makes up for. */
export interface Compensation {cardId: string; points: string; reason: string; invocationId?: string; requests: number}

/** The longest invocation ID an adjustment can name: a card ID (128), a colon and the client's ID (128). */
export const MAX_INVOCATION_ID = 257;
/** An invocation ID the adjustment endpoint takes (1–257 ASCII letters, digits, - _ . :); others are named in the reason only. */
export const linkableInvocation = (id: unknown): id is string => typeof id === 'string' && /^[A-Za-z0-9_.:-]{1,257}$/.test(id);

/** Micro-credits as the points the adjustment form takes, exactly: 2499600 → 2.4996. */
export function microToPoints(micro: number): string {
  const whole = Math.floor(micro / 1_000_000), fraction = String(micro % 1_000_000).padStart(6, '0').replace(/0+$/, '');
  return fraction ? `${whole}.${fraction}` : String(whole);
}

/** One request in a reason: 09-26 15:34 claude-opus-5-5（输出中断）. */
export function requestLabel(request: ChargedRequest, nowMs = Date.now()): string {
  const why = errorClassLabel(request.error_class) || (request.status && request.status !== 'success' ? traceStatusView(request.status).label : '');
  return `${formatListTime(request.ts, nowMs)} ${request.exposed_model || '请求'}${why ? `（${why}）` : ''}`;
}

/**
 * The adjustment that gives back what these requests charged: their charges added up, a reason
 * naming them, and, for a single request, the request it makes up for (an adjustment names one).
 * Null when none of them was charged.
 */
export function compensation(cardId: string, requests: ChargedRequest[], nowMs = Date.now()): Compensation | null {
  const charged = requests.filter(request => Number(request.credits_charged) > 0).sort((a, b) => a.ts - b.ts);
  if (!charged.length) return null;
  const micro = charged.reduce((sum, request) => sum + Math.round(Number(request.credits_charged)), 0);
  if (charged.length === 1) {
    const [request] = charged;
    return {cardId, points: microToPoints(micro), reason: `补偿 ${requestLabel(request, nowMs)}`, requests: 1,
      ...(linkableInvocation(request.invocation_id) ? {invocationId: request.invocation_id} : {})};
  }
  const listed = `补偿 ${charged.length} 次请求：${charged.map(request => requestLabel(request, nowMs)).join('、')}`;
  // A long list is summed up by its first and last time, so the reason stays readable.
  const reason = listed.length <= 200 ? listed
    : `补偿 ${charged.length} 次请求（${formatListTime(charged[0].ts, nowMs)} 至 ${formatListTime(charged[charged.length - 1].ts, nowMs)}）`;
  return {cardId, points: microToPoints(micro), reason, requests: charged.length};
}

/**
 * Why the server would not compensate a request, with what it found: the request is unknown (not
 * in the ledger, its archives or the traces), another card's, already compensated, or charged
 * less than the compensation. The last two can be overridden with a reason (allowRepeat).
 */
export type CompensationRefusal =
  | {kind: 'unknown'; invocationId: string}
  | {kind: 'otherCard'; invocationId: string; cardId: string}
  | {kind: 'repeat'; invocationId: string; charged: string; chargedAt: number; earlier: {credits: string; at: number; operator: string; reason: string}}
  | {kind: 'over'; invocationId: string; charged: string; chargedAt: number; asked: string};

const seconds = (iso: string) => Math.floor(Date.parse(iso) / 1000);
const CHARGED = String.raw`^Request (\S+) was charged (-?[\d.]+) credits at (\S+Z)`;
const REFUSALS: Array<[RegExp, (match: RegExpExecArray) => CompensationRefusal]> = [
  [/^Request (\S+) was not found$/, match => ({kind: 'unknown', invocationId: match[1]})],
  [/^Request (\S+) was made by card (\S+), not \S+$/, match => ({kind: 'otherCard', invocationId: match[1], cardId: match[2]})],
  [new RegExp(String.raw`${CHARGED}, and was already compensated (-?[\d.]+) credits at (\S+Z) by (.*?) \(([\s\S]*)\); send allowRepeat with a reason to compensate it again$`),
    match => ({kind: 'repeat', invocationId: match[1], charged: match[2], chargedAt: seconds(match[3]), earlier: {credits: match[4], at: seconds(match[5]), operator: match[6], reason: match[7]}})],
  [new RegExp(String.raw`${CHARGED}; a compensation of (-?[\d.]+) credits is more than that; send allowRepeat with a reason to compensate more$`),
    match => ({kind: 'over', invocationId: match[1], charged: match[2], chargedAt: seconds(match[3]), asked: match[4]})],
];

/** The server's refusal of a compensation, read; null for any other answer. */
export function compensationRefusal(message: string): CompensationRefusal | null {
  for (const [pattern, read] of REFUSALS) {
    const match = pattern.exec(message.trim());
    if (match) return read(match);
  }
  return null;
}

/** Whether the refusal can be overridden: 仍要补偿, with a reason. */
export const repeatable = (refusal: CompensationRefusal) => refusal.kind === 'repeat' || refusal.kind === 'over';

const credits = (decimal: string) => formatCredits(Number(decimal));

/** What the server found, in a line each: the charge and when, and the earlier compensation. */
export function refusalFacts(refusal: CompensationRefusal, nowMs = Date.now()): string[] {
  switch (refusal.kind) {
    case 'unknown': return [`请求 ${shortId(refusal.invocationId, 'trace')}：账本、归档和调用追踪里都没有它（可能已过保留期）`];
    case 'otherCard': return [`请求 ${shortId(refusal.invocationId, 'trace')} 是卡 ${shortId(refusal.cardId, 'card')} 发出的，不是这张卡`];
    case 'repeat': return [`这次请求 ${formatListTime(refusal.chargedAt, nowMs)} 扣了 ${credits(refusal.charged)} 积分`,
      `已在 ${formatListTime(refusal.earlier.at, nowMs)} 由 ${refusal.earlier.operator === 'unknown' ? '（未记录操作人）' : refusal.earlier.operator} 补偿过 ${credits(refusal.earlier.credits)} 积分（原因：${refusal.earlier.reason === 'no reason' ? '未填写' : refusal.earlier.reason}）`];
    case 'over': return [`这次请求 ${formatListTime(refusal.chargedAt, nowMs)} 扣了 ${credits(refusal.charged)} 积分`, `这次要补偿 ${credits(refusal.asked)} 积分，多于它扣的`];
  }
}

/** The refusal in a sentence: why nothing was given. */
export function refusalTitle(refusal: CompensationRefusal): string {
  switch (refusal.kind) {
    case 'unknown': return '服务器找不到这次请求，没有入账';
    case 'otherCard': return '这次请求不是这张卡的，没有入账';
    case 'repeat': return '这次请求已经补偿过，没有入账';
    case 'over': return '补偿多于这次请求扣的积分，没有入账';
  }
}

/** The most an adjustment's reason may be, in characters (the server takes 512). */
export const MAX_ADJUST_REASON = 500;
/** The reason 仍要补偿 sends: the adjustment's own, and why it is given again; null when too long together. */
export function repeatReason(reason: string, why: string): string | null {
  const text = `${reason.trim()}；仍要补偿：${why.trim()}`;
  return why.trim() && text.length <= MAX_ADJUST_REASON ? text : null;
}
