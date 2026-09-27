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

/** One request a refusal names, with what the server found of it (micro-credits, seconds). */
export interface RefusedRequest {
  invocationId: string;
  /** For another card's request: the card that made it. */
  cardId?: string;
  chargedMicro?: number;
  chargedAt?: number;
  /** For one compensated already: that compensation's amount, time, operator and reason. */
  compensatedMicro?: number;
  compensatedAt?: number;
  operator?: string;
  reason?: string;
}

/**
 * Why the server would not compensate requests, with what it found: a request is unknown (not
 * in the ledger, its archives or the traces), another card's, already compensated, or the
 * compensation is more than they were charged. The last two can be overridden with a reason
 * (allowRepeat). `requests` lists the requests it names; `count` is how many the refusal is
 * about (an answer in words about several names none of them, only their number).
 */
export interface CompensationRefusal {
  kind: 'unknown' | 'otherCard' | 'repeat' | 'over';
  requests: RefusedRequest[];
  count: number;
  askedMicro?: number;
  chargedTotalMicro?: number;
}

const KINDS = ['unknown', 'otherCard', 'repeat', 'over'];
const seconds = (iso: string) => Math.floor(Date.parse(iso) / 1000);

/** Credits as billing writes them in a refusal (3.2, -0.5, 12), in micro-credits, exactly; NaN otherwise. */
export function decimalToMicro(decimal: string): number {
  const match = /^(-?)(\d+)(?:\.(\d{1,6}))?$/.exec(decimal.trim());
  if (!match) return NaN;
  const micro = Number(match[2]) * 1_000_000 + Number((match[3] ?? '').padEnd(6, '0'));
  return match[1] ? -micro : micro;
}

const CHARGED = String.raw`^Request (\S+) was charged (-?[\d.]+) credits at (\S+Z)`;
/** The server's words, for a server that sends no `refusal` object. */
const TEXTS: Array<[RegExp, (match: RegExpExecArray) => CompensationRefusal]> = [
  [/^Request (\S+) was not found$/, match => ({kind: 'unknown', requests: [{invocationId: match[1]}], count: 1})],
  [/^Request (\S+) was made by card (\S+), not \S+$/, match => ({kind: 'otherCard', requests: [{invocationId: match[1], cardId: match[2]}], count: 1})],
  [new RegExp(String.raw`${CHARGED}, and was already compensated (-?[\d.]+) credits at (\S+Z) by (.*?) \(([\s\S]*)\); send allowRepeat with a reason to compensate it again$`),
    match => ({kind: 'repeat', count: 1, requests: [{invocationId: match[1], chargedMicro: decimalToMicro(match[2]), chargedAt: seconds(match[3]),
      compensatedMicro: decimalToMicro(match[4]), compensatedAt: seconds(match[5]), operator: match[6], reason: match[7]}]})],
  [new RegExp(String.raw`${CHARGED}; a compensation of (-?[\d.]+) credits is more than that; send allowRepeat with a reason to compensate more$`),
    match => ({kind: 'over', count: 1, requests: [{invocationId: match[1], chargedMicro: decimalToMicro(match[2]), chargedAt: seconds(match[3])}],
      askedMicro: decimalToMicro(match[4]), chargedTotalMicro: decimalToMicro(match[2])})],
  // Several requests linked to one adjustment.
  [/^(\d+) of the requests were already compensated; send allowRepeat with a reason to compensate them again$/,
    match => ({kind: 'repeat', requests: [], count: Number(match[1])})],
  [/^A compensation of (-?[\d.]+) credits is more than the (-?[\d.]+) credits these (\d+) requests were charged; send allowRepeat with a reason to compensate more$/,
    match => ({kind: 'over', requests: [], count: Number(match[3]), askedMicro: decimalToMicro(match[1]), chargedTotalMicro: decimalToMicro(match[2])})],
];

const whole = (value: unknown): number | undefined => typeof value === 'number' && Number.isSafeInteger(value) ? value : undefined;
const text = (value: unknown, max: number): string | undefined => typeof value === 'string' && value.length <= max ? value : undefined;

/** The server's `refusal` object (every request it names, with what it found of each), read; null when it is not one. */
function structured(value: unknown): CompensationRefusal | null {
  const refusal = value as {kind?: unknown; requests?: unknown; askedMicroCredits?: unknown; chargedTotalMicroCredits?: unknown} | null;
  if (!refusal || typeof refusal !== 'object' || !KINDS.includes(String(refusal.kind)) || !Array.isArray(refusal.requests) || refusal.requests.length > 500) return null;
  const requests: RefusedRequest[] = [];
  for (const item of refusal.requests as Array<Record<string, unknown> | null>) {
    if (!item || typeof item !== 'object' || typeof item.invocationId !== 'string' || !item.invocationId) return null;
    const fields: Array<[keyof RefusedRequest, unknown]> = [['cardId', text(item.cardId, 256)], ['chargedMicro', whole(item.chargedMicroCredits)], ['chargedAt', whole(item.chargedAtSecs)],
      ['compensatedMicro', whole(item.compensatedMicroCredits)], ['compensatedAt', whole(item.compensatedAtSecs)], ['operator', text(item.operator, 256)], ['reason', text(item.reason, 2000)]];
    requests.push(Object.assign({invocationId: item.invocationId}, Object.fromEntries(fields.filter(([, field]) => field !== undefined))));
  }
  const asked = whole(refusal.askedMicroCredits), total = whole(refusal.chargedTotalMicroCredits);
  return {kind: refusal.kind as CompensationRefusal['kind'], requests, count: Math.max(1, requests.length),
    ...(asked !== undefined ? {askedMicro: asked} : {}), ...(total !== undefined ? {chargedTotalMicro: total} : {})};
}

/**
 * The server's refusal of a compensation, read; null for any other answer. The `refusal` object
 * newer servers send beside the error is preferred; the error's words are read otherwise.
 */
export function compensationRefusal(message: string, refusal?: unknown): CompensationRefusal | null {
  const found = refusal === undefined ? null : structured(refusal);
  if (found) return found;
  for (const [pattern, read] of TEXTS) {
    const match = pattern.exec(message.trim());
    if (match) return read(match);
  }
  return null;
}

/** Whether the refusal can be overridden: 仍要补偿, with a reason. */
export const repeatable = (refusal: CompensationRefusal) => refusal.kind === 'repeat' || refusal.kind === 'over';

const credits = (micro: number | undefined) => micro === undefined || !Number.isFinite(micro) ? '—' : formatCredits(micro / 1_000_000);
const earlier = (request: RefusedRequest, nowMs: number) =>
  `已在 ${formatListTime(request.compensatedAt, nowMs)} 由 ${!request.operator || request.operator === 'unknown' ? '（未记录操作人）' : request.operator} `
  + `补偿过 ${credits(request.compensatedMicro)} 积分（原因：${!request.reason || request.reason === 'no reason' ? '未填写' : request.reason}）`;
/** At most this many requests are listed one by one; the rest are counted. */
const LISTED = 5;
const others = (shown: number, total: number) => total > shown ? [`另有 ${total - shown} 次，见这张卡的操作记录`] : [];

/**
 * What the server found, in a line each: the charge and when, and the earlier compensation.
 * `linked`: how many requests the adjustment named.
 */
export function refusalFacts(refusal: CompensationRefusal, linked = 1, nowMs = Date.now()): string[] {
  const {requests} = refusal, [first] = requests, single = linked <= 1 && requests.length <= 1;
  const listed = requests.slice(0, LISTED);
  switch (refusal.kind) {
    case 'unknown': return single && first ? [`请求 ${shortId(first.invocationId, 'trace')}：账本、归档和调用追踪里都没有它（可能已过保留期）`]
      : [`${refusal.count} 次请求在账本、归档和调用追踪里都找不到（可能已过保留期）${listed.length ? `：${listed.map(request => shortId(request.invocationId, 'trace')).join('、')}` : ''}`,
        ...others(listed.length, requests.length)];
    case 'otherCard': return single && first ? [`请求 ${shortId(first.invocationId, 'trace')} 是卡 ${shortId(first.cardId, 'card')} 发出的，不是这张卡`]
      : [...listed.map(request => `请求 ${shortId(request.invocationId, 'trace')} 是卡 ${shortId(request.cardId, 'card')} 发出的，不是这张卡`), ...others(listed.length, requests.length)];
    case 'repeat': return single && first ? [`这次请求 ${formatListTime(first.chargedAt, nowMs)} 扣了 ${credits(first.chargedMicro)} 积分`, earlier(first, nowMs)]
      : [`其中 ${refusal.count} 次已经补偿过${requests.length ? '' : '，见这张卡的操作记录'}`,
        ...listed.map(request => `${formatListTime(request.chargedAt, nowMs)} 扣了 ${credits(request.chargedMicro)} 积分，${earlier(request, nowMs)}`), ...others(listed.length, requests.length)];
    case 'over': return single && first ? [`这次请求 ${formatListTime(first.chargedAt, nowMs)} 扣了 ${credits(first.chargedMicro)} 积分`, `这次要补偿 ${credits(refusal.askedMicro)} 积分，多于它扣的`]
      : [`这 ${Math.max(linked, refusal.count)} 次请求共扣了 ${credits(refusal.chargedTotalMicro)} 积分`, `这次要补偿 ${credits(refusal.askedMicro)} 积分，多于它们扣的`];
  }
}

/** The refusal in a sentence: why nothing was given. `linked`: how many requests the adjustment named. */
export function refusalTitle(refusal: CompensationRefusal, linked = 1): string {
  const single = linked <= 1;
  switch (refusal.kind) {
    case 'unknown': return single ? '服务器找不到这次请求，没有入账' : `服务器找不到其中 ${refusal.count} 次请求，没有入账`;
    case 'otherCard': return single ? '这次请求不是这张卡的，没有入账' : `其中 ${refusal.count} 次请求不是这张卡的，没有入账`;
    case 'repeat': return single ? '这次请求已经补偿过，没有入账' : `其中 ${refusal.count} 次请求已经补偿过，没有入账`;
    case 'over': return single ? '补偿多于这次请求扣的积分，没有入账' : '补偿多于这些请求扣的积分，没有入账';
  }
}

/** The most an adjustment's reason may be, in characters (the server takes 512). */
export const MAX_ADJUST_REASON = 500;
/** The reason 仍要补偿 sends: the adjustment's own, and why it is given again; null when too long together. */
export function repeatReason(reason: string, why: string): string | null {
  const text = `${reason.trim()}；仍要补偿：${why.trim()}`;
  return why.trim() && text.length <= MAX_ADJUST_REASON ? text : null;
}
