// 补偿这次扣费: an adjustment that gives back what one request (or a few) charged, with a reason
// that says which, linked to the request so the card's history can lead back to it. Pure
// functions, so the rules can be tested without a browser.
import {formatListTime} from './format';
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

/** An invocation ID the adjustment endpoint takes (1–128 ASCII letters, digits, - _ . :); others are named in the reason only. */
export const linkableInvocation = (id: unknown): id is string => typeof id === 'string' && /^[A-Za-z0-9_.:-]{1,128}$/.test(id);

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
