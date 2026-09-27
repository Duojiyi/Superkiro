// 调用追踪's filters as the server takes them (GET traces?fromSecs&toSecs&cardId&model&provider&status):
// the time range in local minutes, the same match run again over what an older server returns
// unfiltered, and the totals and failure breakdowns of what is shown. Pure functions, so the
// rules can be tested without a browser.
import type {AdminTrace, TraceTotals} from './api';
import {refusedTrace, TRACE_IN_PROGRESS} from './status';
import type {TraceTab, TraceWindow} from './types';

/** A chosen range as the two inputs hold it (2026-09-26T08:30); an empty one is open. */
export interface TraceSpan {from: string; to: string}
export interface TraceBounds {fromSecs?: number; toSecs?: number}
/** What the server narrows the requests to; `provider` matches the one that answered or any attempted. */
export interface TraceScope {bounds: TraceBounds; cardId?: string; model?: string; provider?: string}

const pad = (value: number) => String(value).padStart(2, '0');

/** A local time as a datetime-local input shows it: 2026-09-26T08:30. */
export const minuteInput = (date: Date) =>
  `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;

/** The start of a local minute given as 2026-09-26T08:30, or null. */
export function minuteStart(text: string): number | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})$/.exec(text);
  if (!match) return null;
  const [year, month, day, hour, minute] = match.slice(1).map(Number);
  const date = new Date(year, month - 1, day, hour, minute);
  return date.getMonth() === month - 1 && date.getDate() === day && hour < 24 && minute < 60 ? Math.floor(date.getTime() / 1000) : null;
}

/**
 * The bounds a range asks the server for: 近 1 小时 and 近 24 小时 back from now; a chosen range from
 * its first minute to the end of its last (both included), either end left open when empty. Null
 * for a chosen range that is not one (an unreadable time, or the end before the start).
 */
export function traceBounds(window: TraceWindow, span: TraceSpan, nowSecs: number): TraceBounds | null {
  if (window === 'hour') return {fromSecs: Math.floor(nowSecs) - 3600};
  if (window === 'day') return {fromSecs: Math.floor(nowSecs) - 86400};
  if (window !== 'custom') return {};
  const from = span.from ? minuteStart(span.from) : undefined, last = span.to ? minuteStart(span.to) : undefined;
  if (from === null || last === null) return null;
  const to = last === undefined ? undefined : last + 60;
  if (from !== undefined && to !== undefined && from >= to) return null;
  return {...(from !== undefined ? {fromSecs: from} : {}), ...(to !== undefined ? {toSecs: to} : {})};
}

/** A chosen range in words: 09-26 08:30 至 09-26 18:00, 09-26 08:30 起, 至 09-26 18:00. */
export function spanText(span: TraceSpan): string {
  const words = (text: string) => text.slice(5).replace('T', ' ');
  if (span.from && span.to) return `${words(span.from)} 至 ${words(span.to)}`;
  return span.from ? `${words(span.from)} 起` : span.to ? `至 ${words(span.to)}` : '全部';
}

/** A request the provider answered, or one it was tried for (its attempts), as the server matches. */
export const involves = (trace: AdminTrace, provider: string) =>
  trace.provider_id === provider || (trace.attempt_chain ?? []).some(attempt => attempt.provider_id === provider);

/** Whether a request is in the scope: what an older server, which ignores the filters, returns is narrowed the same way. */
export function inScope(trace: AdminTrace, {bounds, cardId, model, provider}: TraceScope): boolean {
  const ts = Number(trace.ts);
  return (bounds.fromSecs === undefined || ts >= bounds.fromSecs) && (bounds.toSecs === undefined || ts < bounds.toSecs)
    && (!cardId || trace.card_id === cardId) && (!model || trace.exposed_model === model) && (!provider || involves(trace, provider));
}


/** Whether a request belongs under a status tab: 失败 and 拒绝 split the server's error status. */
export function tabMatches(trace: AdminTrace, tab: TraceTab): boolean {
  switch (tab) {
    case 'ALL': return true;
    case 'in_progress': return TRACE_IN_PROGRESS.includes(String(trace.status));
    case 'error': return trace.status === 'error' && !refusedTrace(trace);
    case 'refused': return refusedTrace(trace);
    default: return trace.status === tab;
  }
}

/** The status the server is asked for under a tab: 失败 and 拒绝 are both its error. */
export const serverStatus = (tab: TraceTab): string | undefined => tab === 'ALL' ? undefined : tab === 'refused' ? 'error' : tab;

/** Requests refused for the card or the request itself among these (the server counts them as failures). */
export const refusedCount = (traces: AdminTrace[]) => traces.filter(refusedTrace).length;

/** Totals as the server counts them: a failure is a request whose status is error. */
export function traceTotals(traces: AdminTrace[]): TraceTotals {
  return traces.reduce((totals, trace) => ({count: totals.count + 1, failures: totals.failures + (trace.status === 'error' ? 1 : 0),
    creditsCharged: totals.creditsCharged + Number(trace.credits_charged ?? 0), costMicroCny: totals.costMicroCny + Number(trace.provider_cost_micro_cny ?? 0)}),
  {count: 0, failures: 0, creditsCharged: 0, costMicroCny: 0});
}

// Most first; of as many, by name, the unnamed ('') last.
const ranked = (counts: Map<string, number>) => [...counts].sort((a, b) => b[1] - a[1] || Number(!a[0]) - Number(!b[0]) || a[0].localeCompare(b[0]));

/**
 * Failed requests by the model asked for, and by provider: each provider whose attempt failed
 * counts the request once; '' for requests refused before anything was sent upstream.
 */
export function failureBreakdown(traces: AdminTrace[]): {models: Array<[string, number]>; providers: Array<[string, number]>} {
  const models = new Map<string, number>(), providers = new Map<string, number>();
  for (const trace of traces) {
    // A refusal for the card or the request itself is not a failure of a model or a provider.
    if (trace.status !== 'error' || refusedTrace(trace)) continue;
    const model = String(trace.exposed_model ?? '');
    models.set(model, (models.get(model) ?? 0) + 1);
    const chain = trace.attempt_chain ?? [];
    const failed = new Set(chain.filter(attempt => !attempt.success).map(attempt => String(attempt.provider_id ?? '')).filter(Boolean));
    // No attempt failed (the answer broke off after a success): the provider that answered.
    if (!failed.size) failed.add(String(trace.provider_id ?? chain[chain.length - 1]?.provider_id ?? ''));
    for (const provider of failed) providers.set(provider, (providers.get(provider) ?? 0) + 1);
  }
  return {models: ranked(models), providers: ranked(providers)};
}

/** A total of charges in credits: 1,234.5 (to four places, as each request's charge). */
export const chargeTotal = (micro: number) => (micro / 1_000_000).toLocaleString('en-US', {maximumFractionDigits: 4});
