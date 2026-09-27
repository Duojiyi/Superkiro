// 模型与定价's list as rules (ModelSheet.tsx draws it): one row per customer model ID across groups, with what the row shows
// (each price table's price and margins, the lowest margin over every route, scheduled prices,
// what a request needs to start) and the filters over it. Pure functions, so the rules can be
// tested without a browser.
import {creditsOfVersion, modelEntries, officialOf, priceState, routeMultiplier, startMicro, type Four, type OfficialInput, type PriceState, type PricingSettings} from './officialPricing';
import {scheduledVersions} from './priceChange';
import {targetsOf} from './routes';

type Row = Record<string, unknown>;

export interface SheetTable {
  rateCardId: string;
  /** The entries charged from this table, one per group. */
  mappings: Row[];
  state: PriceState;
  /** Prices of this model that start later, soonest first. */
  scheduled: Row[];
}

export interface SheetFacts {
  id: string;
  mappings: Row[];
  tables: SheetTable[];
  /** The first table's price in force, as computed from an official price, if it was. */
  official: OfficialInput | null;
  /** The lowest margin over every route of every table, at the model's sample request. */
  worst: number | null;
  /** Priced some other way than from an official price (or not priced at all). */
  unofficial: boolean;
  /** A route billing cannot cost yet: no 成本倍率 for it, its provider or by default. */
  noCost: boolean;
  scheduled: boolean;
  /** Micro-credits a request needs before it starts (the first table's price, the first entry's multipliers). */
  start: number | null;
}

/**
 * Each customer model and what its row shows, from the configuration as it will be published
 * (`models`, the draft) and as it is priced now (`versions`, `settings`). `sample` is the model's
 * typical request; a request reserves its input (at most the context window) and its full output.
 */
export function sheetFacts(models: Row[], context: {groups: Row[]; versions: Row[]; settings: PricingSettings; nowSecs: number; sample: (model: string) => Four}): SheetFacts[] {
  return modelEntries(models, context.groups).map(entry => {
    const byTable = new Map<string, Row[]>();
    for (const mapping of entry.mappings) {
      const rateCardId = context.groups.find(group => group.id === mapping.group_id)?.rate_card_id;
      if (typeof rateCardId === 'string') byTable.set(rateCardId, [...(byTable.get(rateCardId) ?? []), mapping]);
    }
    const tokens = context.sample(entry.id);
    const tables = [...byTable].map(([rateCardId, mappings]): SheetTable => ({rateCardId, mappings,
      state: priceState(context.settings, context.versions, rateCardId, mappings, context.groups, context.nowSecs, tokens),
      scheduled: scheduledVersions(context.versions, rateCardId, [entry.id, mappings[0].target_model], context.nowSecs)}));
    const first = tables[0], margins = tables.flatMap(table => table.state.routes.map(route => route.margin)).filter((value): value is number => value !== null);
    const credits = first ? creditsOfVersion(first.state.version) : null, mapping = entry.mappings[0];
    const input = Math.min(tokens[0] + tokens[2] + tokens[3], Number(mapping.context_window) || Infinity);
    return {id: entry.id, mappings: entry.mappings, tables, official: officialOf(first?.state.version), worst: margins.length ? Math.min(...margins) : null,
      unofficial: !tables.length || tables.some(table => !officialOf(table.state.version)),
      noCost: entry.mappings.some(item => targetsOf(item).some(target => !routeMultiplier(context.settings, target))),
      scheduled: tables.some(table => table.scheduled.length > 0),
      start: credits && Number.isSafeInteger(mapping.max_output) ? startMicro(credits, input, Number(mapping.max_output), first.state.multiplier) : null};
  });
}

export interface SheetFilters {
  loss: boolean;
  /** Margin below this fraction (0.2 for 20%), or null when not filtered. */
  below: number | null;
  unofficial: boolean;
  noCost: boolean;
  scheduled: boolean;
}

/** Whether a row passes every filter that is on. */
export function passes(facts: SheetFacts, filters: SheetFilters): boolean {
  if (filters.loss && !(facts.worst !== null && facts.worst < 0)) return false;
  if (filters.below !== null && !(facts.worst !== null && facts.worst < filters.below)) return false;
  if (filters.unofficial && !facts.unofficial) return false;
  if (filters.noCost && !facts.noCost) return false;
  if (filters.scheduled && !facts.scheduled) return false;
  return true;
}

/** Several entries' states as one: the same everywhere, or 部分在售 when some groups sell it and some do not. */
export function combinedState(mappings: Row[]): 'live' | 'hidden' | 'retired' | 'mixed' {
  const states = new Set(mappings.map(mapping => mapping.retired === true ? 'retired' : mapping.visible === false ? 'hidden' : 'live'));
  return states.size === 1 ? [...states][0] as 'live' | 'hidden' | 'retired' : 'mixed';
}

const tokenText = (value: unknown) => typeof value !== 'number' || !Number.isFinite(value) ? '空' : value >= 1_000_000 ? `${+(value / 1_000_000).toFixed(1)}M` : value >= 1000 ? `${+(value / 1000).toFixed(1)}K` : String(value);
const shown = (value: unknown) => value === undefined || value === null || value === '' ? '空' : typeof value === 'boolean' ? (value ? '开' : '关') : String(value);
const FIELDS: Array<[string, string, (value: unknown) => string]> = [
  ['exposed_model_id', '模型 ID', shown], ['display_name', '显示名', shown], ['description', '说明', shown], ['rate_multiplier', '显示倍率', shown],
  ['context_window', '上下文', tokenText], ['max_output', '最大输出', tokenText], ['credit_multiplier', '模型倍率', shown], ['visible', '客户可见', shown],
  ['supports_tools', '工具', shown], ['supports_vision', '图片', shown], ['supports_reasoning', '推理', shown]];

/**
 * What a publication changes in one model entry, field by field, in the words confirmations use
 * ("新增备用 测试供应商 / claude-sonnet-5（不能服务）"), and the routes it adds that cannot serve.
 * `serves` says whether a provider's enabled Keys can serve a model; `provider` names a provider.
 */
export function entryChanges(before: Row | undefined, after: Row, context: {serves: (target: {provider_id: string; target_model: string}) => boolean; provider: (id: string) => string}): {lines: string[]; unservable: string[]} {
  const lines: string[] = [], unservable: string[] = [];
  const route = (target: {provider_id: string; target_model: string}) => `${context.provider(target.provider_id)} / ${target.target_model}`;
  const flagged = (target: {provider_id: string; target_model: string}) => {
    if (context.serves(target)) return route(target);
    unservable.push(route(target));
    return `${route(target)}（不能服务）`;
  };
  if (!before) return {lines: [`新增条目：主线路 ${flagged(targetsOf(after)[0])}`], unservable};
  for (const [field, label, text] of FIELDS) if (JSON.stringify(before[field] ?? null) !== JSON.stringify(after[field] ?? null)) lines.push(`${label} ${text(before[field])} → ${text(after[field])}`);
  const [wasPrimary, ...wasBackups] = targetsOf(before), [primary, ...backups] = targetsOf(after);
  const key = (target: {provider_id: string; target_model: string}) => `${target.provider_id}\n${target.target_model}`;
  if (key(wasPrimary) !== key(primary)) lines.push(`主线路 ${route(wasPrimary)} → ${flagged(primary)}`);
  const had = new Set([wasPrimary, ...wasBackups].map(key)), has = new Set([primary, ...backups].map(key));
  for (const backup of backups) if (!had.has(key(backup))) lines.push(`新增备用 ${flagged(backup)}`);
  for (const backup of wasBackups) if (!has.has(key(backup))) lines.push(`去掉备用 ${route(backup)}`);
  const kept = (list: Array<{provider_id: string; target_model: string}>) => list.map(key).filter(item => had.has(item) && has.has(item)).join('\n');
  if (kept(wasBackups) !== kept(backups) && !lines.some(line => line.startsWith('主线路'))) lines.push('备用线路换了顺序');
  const aliases = (row: Row) => Array.isArray(row.aliases) ? row.aliases.map(String) : [];
  for (const alias of aliases(after)) if (!aliases(before).includes(alias)) lines.push(`新增别名 ${alias}`);
  for (const alias of aliases(before)) if (!aliases(after).includes(alias)) lines.push(`去掉别名 ${alias}`);
  if (Number(before.sort_order ?? 0) !== Number(after.sort_order ?? 0)) lines.push(`排序位置 ${Number(before.sort_order ?? 0)} → ${Number(after.sort_order ?? 0)}`);
  return {lines, unservable};
}
