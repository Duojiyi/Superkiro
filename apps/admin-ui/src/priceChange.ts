// One-step price changes: which version prices a model now, a new version built from the
// typed prices (exact integer micro-credits, via pricing.ts), and what a sample request would
// cost the customer and us. Pure functions, so the rules can be tested without a browser.
import {formatMicroPrice, previewFixedCharge, priceToMicroPerMillion} from './pricing';

type Row = Record<string, unknown>;

/** Customer prices, in the order billing adds them up: input, output, cache write, cache read. */
export const PRICE_FIELDS = [
  ['fixed_input_credit_per_m', '输入'],
  ['fixed_output_credit_per_m', '输出'],
  ['fixed_cache_creation_credit_per_m', '缓存写'],
  ['fixed_cache_read_credit_per_m', '缓存读'],
] as const;

/** Procurement prices per million tokens, in the version's currency, same order. */
export const COST_FIELDS = [
  ['input_price_per_m', '输入'],
  ['output_price_per_m', '输出'],
  ['cache_creation_price_per_m', '缓存写'],
  ['cache_read_price_per_m', '缓存读'],
] as const;

/** The server's bound: 1,000,000 credits per million tokens. */
export const MAX_PRICE_MICRO = 1_000_000_000_000;

const bytes = (text: string) => new TextEncoder().encode(text).length;
const validText = (value: string, max: number) => !!value.trim() && bytes(value) <= max && !/[\x00-\x1f\x7f-\x9f]/.test(value);
const time = (version: Row) => Number(version.effective_from_secs);

/**
 * The version a new request is priced at now, as billing resolves it: the model's own
 * price, else its upstream model's, else the rate card's `*`.
 */
export function currentVersion(versions: Row[], rateCardId: unknown, names: unknown[], nowSecs: number): Row | null {
  const latest = (name: unknown) => versions
    .filter(version => version.rate_card_id === rateCardId && version.model === name && time(version) <= nowSecs)
    .sort((a, b) => time(b) - time(a))[0] ?? null;
  for (const name of names) {
    if (typeof name !== 'string' || !name || name === '*') continue;
    const found = latest(name);
    if (found) return found;
  }
  return latest('*');
}

/** The name a provider's own procurement cost for an upstream model is kept under: `<provider>/<upstream model>`. */
export const routeCostModel = (providerId: string, targetModel: string) => `${providerId}/${targetModel}`;

/** The provider and upstream model a version records the procurement cost of, or null for a customer price. */
export function routeCostOf(version: Row, providers: Row[]): {provider: Row; target: string} | null {
  const model = String(version.model ?? '');
  const provider = providers.filter(item => model.startsWith(`${String(item.id)}/`)).sort((a, b) => String(b.id).length - String(a.id).length)[0];
  return provider ? {provider, target: model.slice(String(provider.id).length + 1)} : null;
}

export type CostSource = 'route' | 'upstream' | 'wildcard' | 'model';

/**
 * What a request served by this target costs us, found as billing finds it: the version kept for
 * this provider and upstream model (线路采购价), the upstream model's, the table's `*`, and — for
 * the model's own primary target — the model's price version.
 */
export function routeCost(versions: Row[], rateCardId: unknown, target: {provider_id: string; target_model: string}, model: Row | null, nowSecs: number): {version: Row | null; source: CostSource | null} {
  const latest = (name: string) => versions.filter(version => version.rate_card_id === rateCardId && version.model === name && time(version) <= nowSecs)
    .sort((a, b) => time(b) - time(a))[0] ?? null;
  const steps: Array<[CostSource, string]> = [['route', routeCostModel(target.provider_id, target.target_model)], ['upstream', target.target_model], ['wildcard', '*']];
  for (const [source, name] of steps) {const version = latest(name); if (version) return {version, source};}
  if (model && model.target_provider_id === target.provider_id && model.target_model === target.target_model) {
    const own = currentVersion(versions, rateCardId, [model.exposed_model_id, model.target_model], nowSecs);
    if (own) return {version: own, source: 'model'};
  }
  return {version: null, source: null};
}

/** A version's procurement prices in one line: USD 3 / 15 / 3.75 / 0.3 (input, output, cache write, cache read). */
export function costText(version: Row | null): string {
  if (!version || !['USD', 'CNY'].includes(String(version.currency))) return '—';
  const values = COST_FIELDS.map(([field]) => typeof version[field] === 'number' && Number.isFinite(version[field]) && Number(version[field]) >= 0 ? String(version[field]) : '—');
  return `${String(version.currency)} ${values.join(' / ')}`;
}

/**
 * 线路采购价: what one provider charges for one upstream model, kept as its own version: credit
 * prices 0 (never used to charge), effective at once (0) — made later when published, if the
 * table already has one (see timeDraftVersions). Throws with the reason, in the words shown.
 */
export function buildRouteCost(input: {costs: Record<string, string>; currency: string}, context: {providerId: string; targetModel: string; rateCardId: string; nowSecs: number; taken: unknown[]}): Row {
  if (!['USD', 'CNY'].includes(input.currency)) throw new Error('请选择采购价币种');
  const costs = Object.fromEntries(COST_FIELDS.map(([field, label]) => {
    const raw = (input.costs[field] ?? '').trim(), value = Number(raw);
    if (!raw || !Number.isFinite(value) || value < 0 || value > 1_000_000) throw new Error(`${label}采购价需在 0–1,000,000 之间（免费填 0）`);
    return [field, value];
  }));
  const model = routeCostModel(context.providerId, context.targetModel);
  return {id: versionIdFor(model, context.nowSecs, context.taken), rate_card_id: context.rateCardId, model, pricing_mode: 'fixed', currency: input.currency, ...costs,
    ...Object.fromEntries(PRICE_FIELDS.map(([field]) => [field, 0])), per_call_credit: 0, margin_multiplier: 1, effective_from_secs: 0};
}

/**
 * Draft versions as they are sent: one marked 0 (now) whose table already has a version of that
 * model cannot start now (the server refuses it), so it starts at `laterSecs` instead.
 */
export function timeDraftVersions(draft: Row[], versions: Row[], laterSecs: number): Row[] {
  return draft.map(version => version.effective_from_secs === 0 && versions.some(other => other.rate_card_id === version.rate_card_id && other.model === version.model)
    ? {...version, effective_from_secs: laterSecs} : version);
}

/** Versions of these model names that start later, soonest first. */
export function scheduledVersions(versions: Row[], rateCardId: unknown, names: unknown[], nowSecs: number): Row[] {
  return versions.filter(version => version.rate_card_id === rateCardId && names.includes(version.model) && time(version) > nowSecs)
    .sort((a, b) => time(a) - time(b));
}

const pad = (value: number) => String(value).padStart(2, '0');

/** `model-yyyyMMddHHmm` (local time of the moment it takes effect), then -2, -3… if taken. */
export function versionIdFor(model: string, effectiveSecs: number, taken: Iterable<unknown>): string {
  const date = new Date(effectiveSecs * 1000);
  const stamp = `${date.getFullYear()}${pad(date.getMonth() + 1)}${pad(date.getDate())}${pad(date.getHours())}${pad(date.getMinutes())}`;
  const used = new Set(Array.from(taken, String));
  let head = model.trim() || 'price';
  while (head.length > 1 && bytes(`${head}-${stamp}-99`) > 128) head = head.slice(0, -1);
  let id = `${head}-${stamp}`;
  for (let next = 2; used.has(id); next++) id = `${head}-${stamp}-${next}`;
  return id;
}

/** A signed decimal as typed ("1.2", "-10", "+5"; the minus sign shown in changes too), exactly: n / d. */
function decimal(text: string): {n: bigint; d: bigint} | null {
  const match = /^([+\-−]?)(\d{1,9})(?:\.(\d{1,9}))?$/.exec(text.trim());
  if (!match) return null;
  const fraction = match[3] ?? '', n = BigInt(match[2] + fraction);
  return {n: match[1] && match[1] !== '+' ? -n : n, d: 10n ** BigInt(fraction.length)};
}

/** 批量调价: a price × a factor, or ± a percentage, exactly, rounded half up to one micro-credit. */
export function scaledPrice(micro: number, how: 'factor' | 'percent', value: string): number {
  const parsed = decimal(value);
  if (!parsed) throw new Error(how === 'factor' ? '系数须为正数，最多 9 位小数' : '百分比须为数字，如 -10 或 5');
  const {n, d} = how === 'factor' ? parsed : {n: 100n * parsed.d + parsed.n, d: 100n * parsed.d};
  if (n <= 0n) throw new Error(how === 'factor' ? '系数须大于 0' : '降价不能达到或超过 100%');
  const result = (2n * BigInt(micro) * n + d) / (2n * d);
  if (result > BigInt(MAX_PRICE_MICRO)) throw new Error('售价最多 1,000,000 积分 / 百万 Tokens');
  return Number(result);
}

/** "+25%", "−20%", "—" when unchanged, "新" when there was no price before. */
export function percentChange(before: number | null, after: number | null): string {
  if (after === null) return '';
  if (before === null || (before === 0 && after !== 0)) return '新';
  if (before === after) return '—';
  const change = (after - before) / before * 100;
  const shown = Math.abs(change) < 10 ? Math.round(Math.abs(change) * 10) / 10 : Math.round(Math.abs(change));
  return `${change > 0 ? '+' : '−'}${shown}%`;
}

/** A stored price, shown as credits per million tokens; null when it cannot be shown safely. */
export function creditsText(value: unknown): string | null {
  try {return typeof value === 'number' ? formatMicroPrice(value) : null;} catch {return null;}
}

export interface PriceInput {
  /** Credits per million tokens, as typed, per PRICE_FIELDS. */
  prices: Record<string, string>;
  /** Procurement per million tokens, as typed, per COST_FIELDS. */
  costs: Record<string, string>;
  currency: string;
  multiplier: string;
  effectiveSecs: number;
  id: string;
}

/**
 * Checks the typed values and builds the version to publish on top of the current one (so
 * nothing else it carries is lost). Throws with the reason, in the words the drawer shows.
 */
export function buildPriceVersion(input: PriceInput, context: {model: string; rateCardId: string; versions: Row[]; nowSecs: number; base?: Row | null}): Row {
  const id = input.id.trim();
  if (!validText(id, 128)) throw new Error('版本 ID 无效：不超过 128 字节，不含控制字符');
  if (context.versions.some(version => version.id === id)) throw new Error('版本 ID 已存在，请换一个');
  // 0 is "now", which the server accepts only for a model the price table has no version of yet.
  const priced = context.versions.some(version => version.rate_card_id === context.rateCardId && version.model === context.model);
  if (input.effectiveSecs === 0 && priced) throw new Error('这个模型在价格表里已有价格：新价格只能定在将来生效');
  if (input.effectiveSecs !== 0 && (!Number.isSafeInteger(input.effectiveSecs) || input.effectiveSecs <= context.nowSecs)) throw new Error('生效时间需晚于现在，不能追溯生效');
  if (context.versions.some(version => version.rate_card_id === context.rateCardId && version.model === context.model && time(version) === input.effectiveSecs)) {
    throw new Error('这一时刻已有这个模型的价格版本，请换一个生效时间');
  }
  const multiplier = Number(input.multiplier);
  if (!input.multiplier.trim() || !Number.isFinite(multiplier) || multiplier <= 0 || multiplier > 1000) throw new Error('版本倍率需大于 0、不超过 1000');
  if (!['USD', 'CNY'].includes(input.currency)) throw new Error('请选择采购价币种');
  const prices = Object.fromEntries(PRICE_FIELDS.map(([field, label]) => {
    let micro: number;
    try {micro = priceToMicroPerMillion((input.prices[field] ?? '').trim(), 'million');}
    catch {throw new Error(`${label}售价须为非负数，最多 6 位小数`);}
    if (micro > MAX_PRICE_MICRO) throw new Error(`${label}售价最多 1,000,000 积分 / 百万 Tokens`);
    return [field, micro];
  }));
  const costs = Object.fromEntries(COST_FIELDS.map(([field, label]) => {
    const raw = (input.costs[field] ?? '').trim(), value = Number(raw);
    if (!raw || !Number.isFinite(value) || value < 0 || value > 1_000_000) throw new Error(`${label}采购价需在 0–1,000,000 之间（免费填 0）`);
    return [field, value];
  }));
  return {
    per_call_credit: 0, ...(context.base ?? {}),
    id, rate_card_id: context.rateCardId, model: context.model, pricing_mode: 'fixed', currency: input.currency,
    ...costs, ...prices, margin_multiplier: multiplier, effective_from_secs: input.effectiveSecs,
  };
}

export interface SampleCost {
  /** What the customer is charged, in micro-credits (billing's exact rules). */
  credits: number;
  /** The same in yuan, at the credit face value. */
  yuan: number | null;
  /** What the upstream charges us, in yuan. */
  costYuan: number | null;
  /** (yuan − cost) / yuan, in percent. */
  marginPct: number | null;
}

/** A sample request: its charge, what that is in yuan, what it costs us, and the margin. */
export function sampleCost(input: {rates: number[]; tokens: string[]; multipliers: number[]; faceValueCny?: unknown; costs?: number[]; currency?: unknown; usdCnyRate?: unknown}): SampleCost {
  const credits = previewFixedCharge(input.rates, input.tokens, input.multipliers);
  const face = Number(input.faceValueCny);
  const yuan = typeof input.faceValueCny === 'number' && Number.isFinite(face) && face >= 0 ? credits / 1_000_000 * face : null;
  const rate = input.currency === 'CNY' ? 1 : input.currency === 'USD' ? Number(input.usdCnyRate) : NaN;
  const costYuan = input.costs && input.costs.length === 4 && input.costs.every(value => Number.isFinite(value) && value >= 0) && Number.isFinite(rate) && rate > 0
    ? input.costs.reduce((sum, perMillion, index) => sum + Number(input.tokens[index]) * perMillion / 1_000_000, 0) * rate
    : null;
  const marginPct = yuan !== null && costYuan !== null && yuan > 0 ? (yuan - costYuan) / yuan * 100 : null;
  return {credits, yuan, costYuan, marginPct};
}
