// Health by attempt: each upstream attempt of every request, by provider and by Key, so a primary
// failing over to its backup shows its failures although the backup answered; and requests by the
// model customers asked for, to name the models that fail. The server reports these in its stats
// (billing's attempt_activity); for an older server they are counted the same way from the traces
// the console has. Pure functions, so the rules can be tested without a browser.
import {formatCount, formatPercent} from './format';
import {errorClassLabel, failureLabel} from './status';

export interface AttemptWindow {attempts: number; failures: number; takenOver: number; failuresByKind: Record<string, number>}
export interface ModelHealthWindow {requests: number; failures: number; lastFailureAt: number | null; topFailureKind: string | null; failuresByKind: Record<string, number>}
export interface ProviderAttempts {providerId: string; last1h: AttemptWindow; last24h: AttemptWindow; last7d: AttemptWindow}
export interface KeyAttempts extends ProviderAttempts {keyId: string}
export interface ModelHealth {model: string; last1h: ModelHealthWindow; last24h: ModelHealthWindow; last7d: ModelHealthWindow}

interface Attempt {provider_id?: string; key_id?: string; success?: boolean; error?: string | null}
interface Trace {ts: number; attempt_chain?: Attempt[]}

const empty = (): AttemptWindow => ({attempts: 0, failures: 0, takenOver: 0, failuresByKind: {}});

/**
 * Attempts over the last `windowSecs`, by provider and by Key, from traces: every attempt counts,
 * whether or not its provider answered in the end; a failed one is taken over when a later attempt
 * by another provider succeeded.
 */
export function attemptsFromTraces(traces: Trace[], nowSecs: number, windowSecs = 86400): {providers: Map<string, AttemptWindow>; keys: Map<string, AttemptWindow>} {
  const providers = new Map<string, AttemptWindow>(), keys = new Map<string, AttemptWindow>();
  const count = (map: Map<string, AttemptWindow>, id: string, attempt: Attempt, takenOver: boolean) => {
    const window = map.get(id) ?? empty();
    window.attempts++;
    if (!attempt.success) {
      window.failures++;
      if (takenOver) window.takenOver++;
      const kind = attempt.error || 'unknown';
      window.failuresByKind[kind] = (window.failuresByKind[kind] ?? 0) + 1;
    }
    map.set(id, window);
  };
  for (const trace of traces) {
    if (!(trace.ts > nowSecs - windowSecs && trace.ts <= nowSecs)) continue;
    const chain = trace.attempt_chain ?? [];
    chain.forEach((attempt, index) => {
      const takenOver = chain.slice(index + 1).some(later => later.success && later.provider_id !== attempt.provider_id);
      if (attempt.provider_id) count(providers, attempt.provider_id, attempt, takenOver);
      if (attempt.key_id) count(keys, attempt.key_id, attempt, takenOver);
    });
  }
  return {providers, keys};
}

/** A failure kind in words: an error class (输出中断) or an attempt's kind (HTTP 529 · 上游过载, 超时). */
export function failureKindLabel(kind: string): string {
  const named = errorClassLabel(kind);
  return named && named !== kind ? named : failureLabel(kind) || kind;
}

/** Failures by kind, most first: HTTP 529 · 上游过载 × 5、超时 × 2. */
export function kindsText(byKind: Record<string, number> | undefined, limit = 2): string {
  const kinds = Object.entries(byKind ?? {}).filter(([, count]) => count > 0).sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]));
  return kinds.slice(0, limit).map(([kind, count]) => `${failureKindLabel(kind)} × ${formatCount(count)}`).join('、') + (kinds.length > limit ? ` 等 ${kinds.length} 种` : '');
}

/** A failure rate's colour, as the success rate's: red from 10% failed, amber from 5%. */
export function failureTone(failures: number, attempts: number): 'danger' | 'warning' | undefined {
  if (!attempts) return undefined;
  const rate = failures / attempts * 100;
  return rate >= 10 ? 'danger' : rate >= 5 ? 'warning' : undefined;
}

/** 7（5.8%）: failures and their share of the attempts or requests. */
export const failureText = (failures: number, total: number) => total ? `${formatCount(failures)}（${formatPercent(failures / total * 100)}）` : '—';

export interface FailingModel {model: string; text: string; window: 'hour' | 'day'}

/**
 * Models customers ask for that fail, for 需要关注, busiest failures first: those that failed in the
 * last hour (claude-opus-5-5 近 1 小时 7 次失败), and those whose every request of the last 24 hours
 * failed (at least three of them). Refusals for a card's own limits say nothing about a model and
 * are not counted by the server.
 */
export function failingModels(health: ModelHealth[]): FailingModel[] {
  const found: Array<FailingModel & {failures: number}> = [];
  for (const entry of health) {
    const hour = entry.last1h, day = entry.last24h;
    const why = (window: ModelHealthWindow) => window.topFailureKind ? `：${failureKindLabel(window.topFailureKind)}` : '';
    if (hour?.failures > 0) {
      const all = hour.failures === hour.requests;
      found.push({model: entry.model, window: 'hour', failures: hour.failures,
        text: `${entry.model} 近 1 小时 ${formatCount(hour.failures)} 次失败${all ? '（全部失败）' : `（共 ${formatCount(hour.requests)} 次）`}${why(hour)}`});
    } else if (day?.requests >= 3 && day.failures === day.requests) {
      found.push({model: entry.model, window: 'day', failures: day.failures, text: `${entry.model} 近 24 小时 ${formatCount(day.requests)} 次请求全部失败${why(day)}`});
    }
  }
  return found.sort((a, b) => b.failures - a.failures || a.model.localeCompare(b.model)).map(({failures: _, ...rest}) => rest);
}
