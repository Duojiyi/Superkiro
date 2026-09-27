// 定价: the one formula every screen uses, and the plans a pricing change publishes. A price comes
// from an official list price in USD per million tokens: our price ¥/M = official × 计费倍率 × ¥
// per official $1; credits/M = that ÷ 积分面值 (stored as micro-credits); a route's cost ¥/M = what
// its upstream bills (its own basis, else the official price) × 成本倍率 × ¥ per official $1. The
// server checks a version's credits and costs against exactly this arithmetic (f64, in this
// order), and costs each request at the route that served it the same way, so what the console
// shows is what billing does. Pure functions, so the rules can be tested without a browser.
import {COST_FIELDS, currentVersion, MAX_PRICE_MICRO, PRICE_FIELDS, routeCost, versionIdFor, type CostSource} from './priceChange';
import {targetsOf, type Target} from './routes';

type Row = Record<string, unknown>;
/** Input, output, cache write (5-minute), cache read: the order billing adds them up. */
export type Four = [number, number, number, number];

export const KINDS = ['输入', '输出', '缓存写', '缓存读'] as const;
export const OFFICIAL_FIELDS = ['input_usd_per_m', 'output_usd_per_m', 'cache_creation_usd_per_m', 'cache_read_usd_per_m'] as const;
/** The card plans issued today (points and ¥), for when the server does not name them. */
export const PLANS = [{name: 'PRO', points: 1000, price: 30}, {name: 'PRO+', points: 2000, price: 55}, {name: 'PRO Max', points: 5000, price: 130}, {name: 'Power', points: 10000, price: 250}];
/** A request with no traces to learn from: 1,000 input and 1,000 output tokens. */
export const DEFAULT_SAMPLE: Four = [1000, 1000, 0, 0];

const finite = (value: unknown): value is number => typeof value === 'number' && Number.isFinite(value);
const own = (map: object, key: string) => Object.prototype.hasOwnProperty.call(map, key);
const object = (value: unknown): Row => value && typeof value === 'object' && !Array.isArray(value) ? value as Row : {};
/** Four finite numbers, or null. */
export const four = (value: unknown): Four | null => Array.isArray(value) && value.length === 4 && value.every(finite) ? [value[0], value[1], value[2], value[3]] : null;
export const multiplierOk = (value: number) => Number.isFinite(value) && value > 0 && value <= 100;
/** What the settings hold at most (billing's MAX_OFFICIAL_PRICES, MAX_ROUTE_COSTS, MAX_PROVIDER_MULTIPLIERS). */
export const MAX_OFFICIAL_PRICES = 200, MAX_ROUTE_COSTS = 200, MAX_PROVIDER_MULTIPLIERS = 200;
export const usdOk = (value: number) => Number.isFinite(value) && value >= 0 && value <= 10_000;

export interface OfficialPrice {usd: Four; note?: string; updatedAt: number}
export interface RouteCostSetting {costMultiplier: number | null; basis: Four | null}
/** The pricing settings, read defensively: an older server has none of the official ones. */
export interface PricingSettings {
  /** ¥ per credit (积分面值). */
  face: number | null;
  /** ¥ per official $1 (1 unless set). */
  usdCny: number;
  /** 美元汇率: what older cost-plus prices in USD are converted at. */
  legacyRate: number | null;
  defaultPrice: number | null;
  defaultCost: number | null;
  providers: Record<string, number>;
  official: Record<string, OfficialPrice>;
  routes: Record<string, RouteCostSetting>;
}

export function readSettings(value: unknown): PricingSettings {
  const settings = object(value);
  const providers: Record<string, number> = {};
  for (const [id, multiplier] of Object.entries(object(settings.provider_cost_multipliers))) if (finite(multiplier)) providers[id] = multiplier;
  const official: Record<string, OfficialPrice> = {};
  for (const [name, entry] of Object.entries(object(settings.official_prices))) {
    const row = object(entry), usd = four(OFFICIAL_FIELDS.map(field => row[field]));
    if (usd) official[name] = {usd, ...(typeof row.note === 'string' && row.note ? {note: row.note} : {}), updatedAt: finite(row.updated_at_secs) ? row.updated_at_secs : 0};
  }
  const routes: Record<string, RouteCostSetting> = {};
  for (const [key, entry] of Object.entries(object(settings.route_costs))) {
    const row = object(entry);
    routes[key] = {costMultiplier: finite(row.cost_multiplier) ? row.cost_multiplier : null, basis: four(row.basis_usd_per_m)};
  }
  return {
    face: finite(settings.credit_face_value_cny) ? settings.credit_face_value_cny : null,
    usdCny: finite(settings.official_usd_cny) ? settings.official_usd_cny : 1,
    legacyRate: finite(settings.usd_cny_rate) ? settings.usd_cny_rate : null,
    defaultPrice: finite(settings.default_price_multiplier) ? settings.default_price_multiplier : null,
    defaultCost: finite(settings.default_cost_multiplier) ? settings.default_cost_multiplier : null,
    providers, official, routes,
  };
}

/** Micro-credits per million tokens, as the server checks them: official × 计费倍率 × ¥ per $1 ÷ 积分面值 × 1,000,000, in this order in f64, rounded half up. */
export const creditsFor = (usd: number, priceMultiplier: number, usdCny: number, face: number) => Math.round(usd * priceMultiplier * usdCny / face * 1_000_000);
/** Our price, ¥ per million tokens. */
export const yuanFor = (usd: number, priceMultiplier: number, usdCny: number) => usd * priceMultiplier * usdCny;
/** A cost, ¥ per million tokens, as the server records and checks it: basis × 成本倍率 × ¥ per $1. */
export const costFor = (basisUsd: number, costMultiplier: number, usdCny: number) => basisUsd * costMultiplier * usdCny;

/** What a price computed from an official one is made of. */
export interface OfficialInput {
  /** Official USD per million tokens: input, output, cache write, cache read. */
  official: Four;
  priceMultiplier: number;
  costMultiplier: number;
  /** What the upstream bills instead of the official price, when it does. */
  basis: Four | null;
  usdCny: number;
  face: number;
}

export const creditsOf = (input: OfficialInput): Four => input.official.map(usd => creditsFor(usd, input.priceMultiplier, input.usdCny, input.face)) as Four;
export const costsOf = (input: OfficialInput): Four => (input.basis ?? input.official).map(usd => costFor(usd, input.costMultiplier, input.usdCny)) as Four;

/** Why these inputs cannot be published, in the words shown; '' when they can. */
export function officialProblem(input: OfficialInput): string {
  if (!input.official.every(usdOk)) return '官方价须在 0–10,000 美元 / 百万 Tokens 之间';
  if (!input.official.some(usd => usd > 0)) return '四项官方价不能都是 0';
  if (input.basis && !input.basis.every(usdOk)) return '上游计费基准须在 0–10,000 美元之间';
  if (!multiplierOk(input.priceMultiplier)) return '计费倍率需大于 0、不超过 100';
  if (!multiplierOk(input.costMultiplier)) return '成本倍率需大于 0、不超过 100';
  if (!(input.usdCny > 0 && input.usdCny <= 1000)) return '官方价 $1 折合的人民币需大于 0、不超过 1000';
  if (!(input.face > 0 && input.face <= 1000)) return '积分面值无效，请先在“定价设置”核对';
  if (creditsOf(input).some(credits => credits > MAX_PRICE_MICRO)) return '售价最多 1,000,000 积分 / 百万 Tokens';
  if (costsOf(input).some(cost => cost > 1_000_000)) return '成本最多 1,000,000 元 / 百万 Tokens';
  return '';
}

/** The `official` block a version records. */
export function officialBlock(input: OfficialInput): Row {
  return {...Object.fromEntries(OFFICIAL_FIELDS.map((field, index) => [field, input.official[index]])), price_multiplier: input.priceMultiplier, cost_multiplier: input.costMultiplier,
    ...(input.basis ? {cost_basis_usd_per_m: [...input.basis]} : {}), usd_cny: input.usdCny, credit_face_value_cny: input.face};
}

/**
 * A fixed CNY version computed from an official price, as the server accepts it: 版本倍率 1, no
 * per-call charge, credits and costs from the block. A route's cost (`<provider>/<upstream>`)
 * charges nothing, so its credits are 0.
 */
export function officialVersion(input: OfficialInput, meta: {id: string; rateCardId: string; model: string; effectiveSecs: number; routeCost?: boolean}): Row {
  const credits = meta.routeCost ? [0, 0, 0, 0] : creditsOf(input), costs = costsOf(input);
  return {id: meta.id, rate_card_id: meta.rateCardId, model: meta.model, currency: 'CNY', pricing_mode: 'fixed',
    ...Object.fromEntries(COST_FIELDS.map(([field], index) => [field, costs[index]])),
    ...Object.fromEntries(PRICE_FIELDS.map(([field], index) => [field, credits[index]])),
    per_call_credit: 0, margin_multiplier: 1, effective_from_secs: meta.effectiveSecs, official: officialBlock(input)};
}

/** What a version was computed from, or null for one priced another way. */
export function officialOf(version: Row | null | undefined): OfficialInput | null {
  const block = object(version?.official), official = four(OFFICIAL_FIELDS.map(field => block[field]));
  if (!official || !finite(block.price_multiplier) || !finite(block.cost_multiplier) || !finite(block.usd_cny) || !finite(block.credit_face_value_cny)) return null;
  return {official, priceMultiplier: block.price_multiplier, costMultiplier: block.cost_multiplier, basis: four(block.cost_basis_usd_per_m), usdCny: block.usd_cny, face: block.credit_face_value_cny};
}

/** A version's customer prices (micro-credits per million), or null when it is not a fixed price. */
export function creditsOfVersion(version: Row | null | undefined): Four | null {
  if (!version || version.pricing_mode !== 'fixed') return null;
  const values = four(PRICE_FIELDS.map(([field]) => version[field]));
  return values && values.every(value => Number.isSafeInteger(value) && value >= 0) ? values : null;
}

export const routeKey = (providerId: string, target: string) => `${providerId}/${target}`;
export type MultiplierSource = 'route' | 'provider' | 'default';
/** A route's 成本倍率 as billing finds it: the route's own, else its provider's, else the default. */
export function routeMultiplier(settings: PricingSettings, target: Target): {value: number; source: MultiplierSource} | null {
  const key = routeKey(target.provider_id, target.target_model);
  const route = own(settings.routes, key) ? settings.routes[key].costMultiplier : null;
  if (route !== null) return {value: route, source: 'route'};
  if (own(settings.providers, target.provider_id)) return {value: settings.providers[target.provider_id], source: 'provider'};
  return settings.defaultCost !== null ? {value: settings.defaultCost, source: 'default'} : null;
}
/** What a route's upstream bills, in official USD: the route's own basis, else its upstream model's official price. */
export function routeBasis(settings: PricingSettings, target: Target): {usd: Four; source: 'route' | 'official'} | null {
  const key = routeKey(target.provider_id, target.target_model);
  const basis = own(settings.routes, key) ? settings.routes[key].basis : null;
  if (basis) return {usd: basis, source: 'route'};
  return own(settings.official, target.target_model) ? {usd: settings.official[target.target_model].usd, source: 'official'} : null;
}

export interface RouteCostView {
  /** ¥ per million tokens, or null when it is not known. */
  perM: Four | null;
  /** From the official prices (as billing costs the route now), from price versions (legacy), or unknown. */
  how: 'official' | 'legacy' | null;
  basis?: {usd: Four; source: 'route' | 'official'};
  multiplier?: {value: number; source: MultiplierSource};
  version?: Row;
  legacySource?: CostSource;
}

/**
 * What a request served by `target` costs, found as billing finds it (for requests reserved under
 * these settings): with both a basis and a multiplier, basis × multiplier × ¥ per $1; otherwise
 * the price versions, as before (the route's own cost version, then the model's price, the
 * upstream model's, the table's `*`), USD at the older exchange rate.
 */
export function costOfRoute(settings: PricingSettings, versions: Row[], rateCardId: unknown, target: Target, mapping: Row | null, atSecs: number): RouteCostView {
  const basis = routeBasis(settings, target), multiplier = routeMultiplier(settings, target);
  if (basis && multiplier) return {perM: basis.usd.map(usd => costFor(usd, multiplier.value, settings.usdCny)) as Four, how: 'official', basis, multiplier};
  const found = routeCost(versions, rateCardId, target, mapping, atSecs), version = found.version;
  if (!version) return {perM: null, how: null, ...(basis ? {basis} : {}), ...(multiplier ? {multiplier} : {})};
  const rate = version.currency === 'CNY' ? 1 : version.currency === 'USD' ? settings.legacyRate : null;
  const values = four(COST_FIELDS.map(([field]) => version[field]));
  const perM = rate !== null && values && values.every(value => value >= 0) ? values.map(value => value * rate) as Four : null;
  return {perM, how: perM ? 'legacy' : null, version, legacySource: found.source ?? undefined, ...(basis ? {basis} : {}), ...(multiplier ? {multiplier} : {})};
}

/** What billing charges for these tokens at these prices (micro-credits), rounded up once. */
export function chargeMicro(credits: Four, tokens: Four, multiplier: number): number {
  let base = 0;
  for (let kind = 0; kind < 4; kind++) base += tokens[kind] * credits[kind] / 1_000_000;
  return Math.ceil(base * multiplier);
}
/** What these tokens cost at these ¥ per million. */
export const costYuan = (perM: Four, tokens: Four) => tokens.reduce((sum, count, kind) => sum + count * perM[kind] / 1_000_000, 0);
/** (revenue − cost) ÷ revenue, or null when there is no revenue. */
export const marginOf = (revenue: number, cost: number) => revenue > 0 ? (revenue - cost) / revenue : null;

/**
 * Credits a request must reserve before it starts, as billing reserves it for a fixed price:
 * the input at the dearest input-side price plus the most output at the output price, × the
 * multipliers, rounded up (micro-credits).
 */
export function startMicro(credits: Four, input: number, maxOutput: number, multiplier: number): number {
  const inputCharge = input * Math.max(credits[0], credits[2], credits[3]) / 1_000_000;
  const outputCharge = maxOutput * credits[1] / 1_000_000;
  return Math.ceil((inputCharge + outputCharge) * multiplier);
}

/** 版本倍率 × 分组倍率 × 模型倍率, each 1 unless set. */
export function multipliers(version: Row | null | undefined, group: Row | null | undefined, mapping: Row | null | undefined): {version: number; group: number; model: number; product: number} {
  const value = (raw: unknown) => finite(raw) ? raw : 1;
  const result = {version: value(version?.margin_multiplier), group: value(group?.margin_multiplier), model: value(mapping?.credit_multiplier)};
  return {...result, product: result.version * result.group * result.model};
}

/**
 * A model's typical request: the median of each token kind over its successful requests among the
 * traces given, else 1,000 input and 1,000 output. A trace's input counts cached tokens too; the
 * cached part is taken out when the trace says how much it was.
 */
export function sampleFor(traces: Row[], model: string): {tokens: Four; count: number} {
  const done = traces.filter(trace => trace.exposed_model === model && trace.status === 'success' && finite(trace.input_tokens) && finite(trace.output_tokens) && Number(trace.input_tokens) + Number(trace.output_tokens) > 0);
  if (!done.length) return {tokens: [...DEFAULT_SAMPLE] as Four, count: 0};
  const median = (values: number[]) => [...values].sort((a, b) => a - b)[Math.floor((values.length - 1) / 2)];
  const count = (value: unknown) => finite(value) && value > 0 ? value : 0;
  const read = done.map(trace => count(trace.cache_read_tokens)), write = done.map(trace => count(trace.cache_creation_tokens));
  const uncached = done.map((trace, index) => Math.max(0, Number(trace.input_tokens) - read[index] - write[index]));
  return {tokens: [median(uncached), median(done.map(trace => Number(trace.output_tokens))), median(write), median(read)], count: done.length};
}

/** A customer model and its entries (one per group), the groups in the configuration's order. */
export interface ModelEntry {id: string; mappings: Row[]}
export function modelEntries(models: Row[], groups: Row[]): ModelEntry[] {
  const place = (id: unknown) => {const index = groups.findIndex(group => group.id === id); return index < 0 ? groups.length : index;};
  const ordered = [...models].sort((a, b) => place(a.group_id) - place(b.group_id) || Number(a.sort_order ?? 0) - Number(b.sort_order ?? 0));
  const entries = new Map<string, Row[]>();
  for (const model of ordered) {const id = String(model.exposed_model_id ?? model.id); entries.set(id, [...(entries.get(id) ?? []), model]);}
  return [...entries].map(([id, mappings]) => ({id, mappings}));
}

/**
 * The first time from `at`, a minute at a time, at which none of these price tables has a price
 * of that model: the server keeps one price per table, model and time.
 */
export function freeTime(versions: Row[], prices: Array<[unknown, unknown]>, at: number): number {
  let time = at;
  while (prices.some(([rateCardId, model]) => versions.some(version => version.rate_card_id === rateCardId && version.model === model && Number(version.effective_from_secs) === time))) time += 60;
  return time;
}

/** In force at this time, or scheduled: a version no later one in force has superseded. */
export function isLiveVersion(version: Row, versions: Row[], nowSecs: number): boolean {
  const from = Number(version.effective_from_secs);
  return from > nowSecs || !versions.some(other => other !== version && other.rate_card_id === version.rate_card_id && other.model === version.model
    && Number(other.effective_from_secs) > from && Number(other.effective_from_secs) <= nowSecs);
}

/**
 * A new face value or ¥ per official $1 reprices every price computed from an official one, in
 * one publication: one in force gets a new version from now (0, which the server stamps), a
 * scheduled one is withdrawn and added again, repriced, at its own time. Prices set another way
 * keep their credits (listed as `legacy`).
 */
export function faceValuePlan(versions: Row[], next: {face: number; usdCny: number}, nowSecs: number): {versions: Row[]; cancelled: string[]; repriced: Array<{before: Row; after: Row}>; legacy: Row[]} {
  const taken = versions.map(version => version.id), plan = {versions: [] as Row[], cancelled: [] as string[], repriced: [] as Array<{before: Row; after: Row}>, legacy: [] as Row[]};
  for (const version of versions.filter(item => isLiveVersion(item, versions, nowSecs))) {
    const input = officialOf(version);
    if (!input) {plan.legacy.push(version); continue;}
    if (input.face === next.face && input.usdCny === next.usdCny) continue;
    const scheduled = Number(version.effective_from_secs) > nowSecs, when = scheduled ? Number(version.effective_from_secs) : 0;
    const id = versionIdFor(String(version.model), scheduled ? when : nowSecs, taken);
    taken.push(id);
    if (scheduled) plan.cancelled.push(String(version.id));
    const routeOnly = PRICE_FIELDS.every(([field]) => version[field] === 0);
    const after = officialVersion({...input, face: next.face, usdCny: next.usdCny}, {id, rateCardId: String(version.rate_card_id), model: String(version.model), effectiveSecs: when, routeCost: routeOnly});
    plan.versions.push(after); plan.repriced.push({before: version, after});
  }
  return plan;
}

/**
 * The official price a new price for this model starts from: its price in force's own block, else
 * the official price table's entry for the model, else for its upstream model; null when there is
 * none (the owner prices it by another model).
 */
export function officialStart(settings: PricingSettings, version: Row | null, mapping: Row): {official: Four; from: 'version' | 'model' | 'upstream'; name: string; input?: OfficialInput} | null {
  const input = officialOf(version);
  if (input) return {official: input.official, from: 'version', name: String(version?.model ?? ''), input};
  const id = String(mapping.exposed_model_id ?? ''), upstream = String(mapping.target_model ?? '');
  if (own(settings.official, id)) return {official: settings.official[id].usd, from: 'model', name: id};
  if (own(settings.official, upstream)) return {official: settings.official[upstream].usd, from: 'upstream', name: upstream};
  return null;
}

/**
 * The cost a version records for its primary route, as billing will cost that route from these
 * settings: the route's (or provider's, or default) 成本倍率 on what the route bills; the basis is
 * left out when it is the official price itself. Null multiplier when none is set.
 */
export function primaryCost(settings: PricingSettings, mapping: Row, official: Four): {costMultiplier: number | null; basis: Four | null; source: MultiplierSource | null} {
  const [primary] = targetsOf(mapping);
  const multiplier = routeMultiplier(settings, primary), basis = routeBasis(settings, primary);
  const billed = basis && basis.usd.some((usd, index) => usd !== official[index]) ? basis.usd : null;
  return {costMultiplier: multiplier?.value ?? null, basis: billed, source: multiplier?.source ?? null};
}

export interface RouteState {target: Target; primary: boolean; cost: RouteCostView; margin: number | null}
export interface PriceState {
  version: Row | null;
  credits: Four | null;
  /** 版本倍率 × 分组倍率 × 模型倍率 of the first entry. */
  multiplier: number;
  /** What customers pay, ¥ per million tokens, at the first entry's multipliers. */
  yuanPerM: Four | null;
  routes: RouteState[];
  /** The lowest margin over routes and groups, at the sample request. */
  worst: number | null;
}

/**
 * A model's price in one price table at one time: the version billing resolves, what its entries'
 * customers pay, and each route's cost and margin at the sample request (the lowest over the
 * groups sharing the table, whose multipliers may differ).
 */
export function priceState(settings: PricingSettings, versions: Row[], rateCardId: string, mappings: Row[], groups: Row[], atSecs: number, tokens: Four): PriceState {
  const first = mappings[0];
  const version = currentVersion(versions, rateCardId, [first?.exposed_model_id, first?.target_model], atSecs);
  const credits = creditsOfVersion(version), groupOf = (mapping: Row) => groups.find(group => group.id === mapping.group_id);
  const factor = multipliers(version, groupOf(first), first).product, face = settings.face;
  const yuanPerM = credits && face !== null ? credits.map(value => value / 1_000_000 * face * factor) as Four : null;
  const routes = targetsOf(first ?? {}).map((target, index): RouteState => {
    const cost = costOfRoute(settings, versions, rateCardId, target, first ?? null, atSecs);
    let margin: number | null = null;
    if (credits && face !== null && cost.perM) {
      const spent = costYuan(cost.perM, tokens);
      for (const mapping of mappings) {
        const value = marginOf(chargeMicro(credits, tokens, multipliers(version, groupOf(mapping), mapping).product) / 1_000_000 * face, spent);
        if (value !== null && (margin === null || value < margin)) margin = value;
      }
    }
    return {target, primary: index === 0, cost, margin};
  });
  const margins = routes.map(route => route.margin).filter((value): value is number => value !== null);
  return {version, credits, multiplier: factor, yuanPerM, routes, worst: margins.length ? Math.min(...margins) : null};
}

export interface ImpactRow {
  rateCardId: string;
  /** The customer model ID. */
  model: string;
  /** Its entries charged from this price table, one per group. */
  mappings: Row[];
  before: PriceState;
  after: PriceState;
  /** A scheduled price of this model that would take over after the change. */
  overriddenBy: Row | null;
  changed: boolean;
}

const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);
const rounded = (values: Four | null) => values && values.map(value => Math.round(value * 1e9) / 1e9);

/**
 * Every customer model a change touches, per price table: its credits, what customers pay, and
 * each route's cost and margin without and with the change (`before` and `after`: the settings
 * and versions), both when the change takes effect, so a price already scheduled in between is
 * not taken for part of this change.
 */
export function pricingImpact(before: {settings: PricingSettings; versions: Row[]}, after: {settings: PricingSettings; versions: Row[]}, context: {models: Row[]; groups: Row[]; nowSecs: number; effectiveSecs: number; sample: (model: string) => Four}): ImpactRow[] {
  const rows: ImpactRow[] = [];
  for (const entry of modelEntries(context.models, context.groups)) {
    const tables = new Map<string, Row[]>();
    for (const mapping of entry.mappings) {
      const rateCardId = context.groups.find(group => group.id === mapping.group_id)?.rate_card_id;
      if (typeof rateCardId === 'string') tables.set(rateCardId, [...(tables.get(rateCardId) ?? []), mapping]);
    }
    for (const [rateCardId, mappings] of tables) {
      const tokens = context.sample(entry.id);
      const was = priceState(before.settings, before.versions, rateCardId, mappings, context.groups, context.effectiveSecs, tokens);
      const will = priceState(after.settings, after.versions, rateCardId, mappings, context.groups, context.effectiveSecs, tokens);
      const names = [mappings[0].exposed_model_id, mappings[0].target_model];
      // A price scheduled before this change that still stands (one the change itself withdraws or adds is part of it).
      const overriddenBy = after.versions.filter(version => before.versions.includes(version) && version.rate_card_id === rateCardId && names.includes(version.model) && Number(version.effective_from_secs) > context.effectiveSecs)
        .sort((a, b) => Number(a.effective_from_secs) - Number(b.effective_from_secs))[0] ?? null;
      const changed = !same(was.credits, will.credits) || !same(rounded(was.yuanPerM), rounded(will.yuanPerM))
        || was.routes.some((route, index) => !same(rounded(route.cost.perM), rounded(will.routes[index]?.cost.perM ?? null)) || route.margin !== will.routes[index]?.margin);
      rows.push({rateCardId, model: entry.id, mappings, before: was, after: will, overriddenBy, changed});
    }
  }
  return rows;
}

/**
 * 同时把计费倍率调到 X 保持毛利: when a route's 成本倍率 changes, each official price whose primary
 * route is costed from official prices gets a new version from `effectiveSecs` whose 计费倍率 moves
 * in proportion (4 decimals), so its margin on that route stays as it was.
 */
export function keepMarginPlan(before: PricingSettings, after: PricingSettings, config: {models: Row[]; groups: Row[]; versions: Row[]}, nowSecs: number, effectiveSecs: number): Array<{version: Row; model: string; rateCardId: string; from: number; to: number}> {
  const plan: Array<{version: Row; model: string; rateCardId: string; from: number; to: number}> = [], taken = config.versions.map(version => version.id), done = new Set<string>();
  for (const entry of modelEntries(config.models, config.groups)) for (const mapping of entry.mappings) {
    const rateCardId = config.groups.find(group => group.id === mapping.group_id)?.rate_card_id;
    if (typeof rateCardId !== 'string' || done.has(`${rateCardId}\n${entry.id}`)) continue;
    done.add(`${rateCardId}\n${entry.id}`);
    const version = currentVersion(config.versions, rateCardId, [mapping.exposed_model_id, mapping.target_model], nowSecs), input = officialOf(version);
    const [primary] = targetsOf(mapping), was = routeMultiplier(before, primary), will = routeMultiplier(after, primary);
    if (!input || !version || !was || !will || was.value === will.value || !routeBasis(before, primary) || !routeBasis(after, primary)) continue;
    const to = Math.round(input.priceMultiplier * will.value / was.value * 10_000) / 10_000;
    if (!multiplierOk(to)) continue;
    const at = freeTime(config.versions, [[rateCardId, version.model]], effectiveSecs), id = versionIdFor(String(version.model), at, taken);
    taken.push(id);
    const next = {...input, priceMultiplier: to, costMultiplier: will.value, basis: primaryCost(after, mapping, input.official).basis, usdCny: after.usdCny, face: after.face ?? input.face};
    plan.push({version: officialVersion(next, {id, rateCardId, model: String(version.model), effectiveSecs: at}), model: entry.id, rateCardId, from: input.priceMultiplier, to});
  }
  return plan;
}

/** The largest change among four prices, in percent (0 when none can be compared). */
export function largestChange(before: Four | null, after: Four | null): number {
  if (!before || !after) return 0;
  return Math.max(0, ...before.map((value, index) => value > 0 ? Math.abs(after[index] - value) / value * 100 : after[index] > 0 ? Infinity : 0));
}
