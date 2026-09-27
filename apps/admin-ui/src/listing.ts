// 上架模型: the rules for listing a new model, as pure functions so they can be tested without
// a browser. A listing is one publication: the model in each chosen group (each at its own
// place; shown, or hidden first), together with its first price in each price table, in force at
// once (effective_from_secs 0, which the server accepts only for a model the table has no version
// of yet) and computed from an official price (or, as before, typed in credits). When a table
// already prices that model ID — another group shares it — the model is listed at that price, or
// with a new one from a later time. Either way a customer never sees a model without a price in force.
import {officialProblem, officialVersion, type OfficialInput} from './officialPricing';
import {buildPriceVersion, currentVersion, PRICE_FIELDS, versionIdFor} from './priceChange';
import {canRoute} from './routes';

type Row = Record<string, unknown>;

/** The model IDs (and aliases) the server accepts. */
export const MODEL_ID = /^[A-Za-z0-9._:/-]{1,128}$/;
export const MODEL_ID_RULE = '模型 ID 只能用英文字母、数字和 . _ : / -，最多 128 个字符';

const bytes = (text: string) => new TextEncoder().encode(text).length;
const validText = (value: unknown, max: number): value is string =>
  typeof value === 'string' && !!value.trim() && value === value.trim() && bytes(value) <= max && !/[\x00-\x1f\x7f-\x9f]/.test(value);

const BRANDS: Record<string, string> = {gpt: 'GPT', glm: 'GLM', deepseek: 'DeepSeek', minimax: 'MiniMax'};

/**
 * "claude-opus-5-5" as "Claude Opus 5.5": the name the server shows when none is given (words
 * capitalised, a run of version numbers joined by dots, a snapshot date left out).
 */
export function displayNameFor(modelId: string): string {
  const words: string[] = [];
  for (const part of modelId.split(/[-_ ]/).filter(Boolean)) {
    if (/^\d+$/.test(part)) {
      if (part.length >= 6) continue;
      const last = words.length - 1;
      if (last >= 0 && /^[\d.]+$/.test(words[last]) && words[last].length <= 4) {words[last] += `.${part}`; continue;}
      words.push(part);
      continue;
    }
    words.push(BRANDS[part.toLowerCase()] ?? part.charAt(0).toUpperCase() + part.slice(1));
  }
  return words.join(' ');
}

/** `<provider>-<model>`, then -2, -3… if taken; at most 128 bytes. */
export function mappingIdFor(providerId: string, modelId: string, taken: Iterable<unknown>): string {
  const used = new Set(Array.from(taken, String));
  let head = `${providerId}-${modelId}`;
  while (head.length > 1 && bytes(`${head}-99`) > 128) head = head.slice(0, -1);
  let id = head;
  for (let next = 2; used.has(id); next++) id = `${head}-${next}`;
  return id;
}

const order = (row: Row) => Number(row.sort_order ?? 0);

/** A group's models in list order (ties in the order the server lists them). */
export function groupModels(models: Row[], groupId: unknown): Row[] {
  return models.filter(model => model.group_id === groupId).sort((a, b) => order(a) - order(b));
}

/**
 * Every model, with this one moved one place up, one down, or to the top of its group, and the
 * group numbered 0, 1, 2… in the new order: no tie is left for the server to settle.
 */
export function reorder(models: Row[], id: unknown, to: 'up' | 'down' | 'first'): Row[] {
  const target = models.find(model => model.id === id);
  if (!target) return models;
  const ordered = groupModels(models, target.group_id), from = ordered.indexOf(target);
  const next = ordered.filter(model => model !== target);
  next.splice(to === 'first' ? 0 : to === 'up' ? Math.max(0, from - 1) : Math.min(next.length, from + 1), 0, target);
  const place = new Map(next.map((model, index) => [model, index]));
  return models.map(model => place.has(model) ? {...model, sort_order: place.get(model)} : model);
}

/** The model Kiro uses when a request names none: the first one of the group customers see. */
export const defaultModel = (models: Row[], groupId: unknown) => groupModels(models, groupId).find(model => model.visible !== false && model.retired !== true);

/** A group's models numbered 0, 1, 2… in the order given, so no two share a place: the ones whose number changes. */
export function renumbered(ordered: Row[]): Row[] {
  return ordered.map((model, index) => ({model, index})).filter(({model, index}) => order(model) !== index || model.sort_order === undefined)
    .map(({model, index}) => ({...model, sort_order: index}));
}

/** The price table's own versions of this model ID, and the other groups' models that are charged by them. */
export function sharedPrice(config: {groups: Row[]; models: Row[]; versions: Row[]}, groupId: unknown, modelId: string): {versions: Row[]; groups: Row[]} {
  const rateCardId = config.groups.find(group => group.id === groupId)?.rate_card_id;
  const versions = config.versions.filter(version => version.rate_card_id === rateCardId && version.model === modelId);
  const groups = config.groups.filter(group => group.id !== groupId && group.rate_card_id === rateCardId
    && config.models.some(model => model.group_id === group.id && model.exposed_model_id === modelId));
  return {versions, groups};
}

const thousands = (tokens: number) => `${Math.round(tokens / 1000)}K`;
/** Opus 5.5 thinks on every request, asked or not. */
const alwaysThinks = (model: string) => /opus[-_.]?5[-_.]5/i.test(model);

/** What to warn about before listing: a thinking model with little room to answer, a long context sold at the base price. */
export function listingWarnings(input: {targetModel: string; modelId: string; reasoning: boolean; contextWindow: number | null; maxOutput: number | null}): string[] {
  const warnings: string[] = [], opus = alwaysThinks(input.targetModel) || alwaysThinks(input.modelId);
  if ((input.reasoning || opus) && input.maxOutput !== null && input.maxOutput < 32_000) {
    warnings.push(`${opus ? 'Opus 5.5 总是会思考' : '打开了推理'}，最大输出却不到 32K：思考也算输出，回答容易被截断`);
  }
  const limit = /gpt/i.test(input.targetModel) ? 272_000 : 200_000;
  if (input.contextWindow !== null && input.contextWindow > limit) {
    warnings.push(`上下文 ${thousands(input.contextWindow)} 超过 ${thousands(limit)}：价格没有长上下文档，更长的请求也按这个价格扣费（上游往往按更高的长上下文价收费）`);
  }
  return warnings;
}

export type Place = {at: 'first' | 'last'} | {at: 'after'; id: string};

export interface ListingInput {
  providerId: string;
  targetModel: string;
  /** What customers see and request. */
  modelId: string;
  /** Empty: the server derives one (displayNameFor). */
  displayName: string;
  groupId: string;
  /** Where in the group: first, last, or after the model with this entry ID. */
  place: Place;
  /** Further groups to list it in at the same time, each at its own place; they share the one price. */
  also?: Array<{groupId: string; place: Place}>;
  /** 上架后先隐藏: listed hidden, to be shown later (重新上架). */
  hidden?: boolean;
  contextWindow: number | null;
  maxOutput: number | null;
  tools: boolean;
  vision: boolean;
  reasoning: boolean;
  /** The multiplier the model list shows; empty: the server derives one from the price. */
  rateMultiplier: string;
  /** The model's own charge multiplier (模型倍率). */
  creditMultiplier: string;
  /** Charge the price a table already has for this model ID (another group shares the table). */
  keepPrice: boolean;
  /** The official price the new price is computed from; without it, the credits typed (旧版). */
  official?: OfficialInput | null;
  /** Credits per million tokens, as typed, per PRICE_FIELDS; procurement per COST_FIELDS. */
  prices: Record<string, string>;
  costs: Record<string, string>;
  currency: string;
}

export interface ListingContext {
  config: {groups: Row[]; models: Row[]; versions: Row[]; rate_cards: Row[]};
  providers: Row[];
  keys: Row[];
  nowSecs: number;
  /** When a new price takes effect for a model ID the table already prices (it cannot start now). */
  effectiveSecs: number;
}

export interface Listing {
  /** Everything to publish: the new entries, and each group's models whose place number changes. */
  models: Row[];
  /** The first group's new entry. */
  mapping: Row;
  /** Every new entry, one per group. */
  mappings: Row[];
  /** The first price table's new price, or null when its existing one is kept. */
  version: Row | null;
  /** Every new price, one per price table that needs one. */
  versions: Row[];
  /** Other groups whose model of the same ID is charged by the same price. */
  sharedWith: Row[];
}

/** Checks the form and builds the one publication. Throws with the reason, in the words the drawer shows. */
export function buildListing(input: ListingInput, context: ListingContext): Listing {
  const {config, providers, keys} = context;
  const provider = providers.find(item => item.id === input.providerId);
  if (!provider) throw new Error('请选择供应商');
  if (provider.enabled === false) throw new Error('这个供应商已停用，先在“供应商与 Key”里启用');
  const targetModel = input.targetModel.trim();
  if (!validText(targetModel, 256)) throw new Error('请填写上游模型');
  if (!canRoute(provider.id, targetModel, keys)) {
    throw new Error(`${String(provider.name ?? provider.id)} 的 Key 还没有授权 ${targetModel}：先在“供应商与 Key”里勾选它并保存`);
  }
  const modelId = input.modelId.trim();
  if (!modelId) throw new Error('请填写模型 ID（客户在 Kiro 里看到并请求的）');
  if (!MODEL_ID.test(modelId)) throw new Error(MODEL_ID_RULE);
  const chosen = [{groupId: input.groupId, place: input.place}, ...(input.also ?? [])];
  if (new Set(chosen.map(item => item.groupId)).size !== chosen.length) throw new Error('同一个分组只能选一次');
  const displayName = input.displayName.trim();
  if (displayName && !validText(displayName, 64)) throw new Error('显示名称不超过 64 字节，不含控制字符');
  const {contextWindow, maxOutput} = input;
  if (!Number.isSafeInteger(contextWindow) || !Number.isSafeInteger(maxOutput) || Number(maxOutput) < 1 || Number(maxOutput) > Number(contextWindow) || Number(contextWindow) > 10_000_000) {
    throw new Error('上下文须为不超过 10,000,000 的正整数 Tokens，最大输出须为正整数且不超过上下文');
  }
  const rateText = input.rateMultiplier.trim(), rate = Number(rateText);
  if (rateText && (!Number.isFinite(rate) || rate <= 0 || rate > 1000)) throw new Error('显示倍率需大于 0、不超过 1000；留空按价格自动换算');
  const creditText = input.creditMultiplier.trim(), credit = Number(creditText);
  if (!creditText || !Number.isFinite(credit) || credit <= 0 || credit > 1000) throw new Error('模型倍率需大于 0、不超过 1000（不加倍填 1）');
  const taken = config.models.map(model => model.id), mappings: Row[] = [];
  let moved: Row[] = [];
  for (const {groupId, place} of chosen) {
    const group = config.groups.find(item => item.id === groupId);
    if (!group) throw new Error('请选择分组');
    const groupName = chosen.length > 1 ? ` ${String(group.name ?? group.id)} ` : '这个分组';
    const rateCardId = group.rate_card_id;
    if (typeof rateCardId !== 'string' || !config.rate_cards.some(card => card.id === rateCardId)) throw new Error(`${groupName.trim()}没有价格表，不能上架`);
    const peers = groupModels(config.models, group.id);
    if (peers.some(model => model.exposed_model_id === modelId || (Array.isArray(model.aliases) && model.aliases.includes(modelId)))) {
      throw new Error(`${groupName.trim()}里已经有 ${modelId}：换一个模型 ID，或直接给它调价`);
    }
    const mapping: Row = {
      id: mappingIdFor(String(provider.id), modelId, taken),
      group_id: group.id, exposed_model_id: modelId, target_provider_id: provider.id, target_model: targetModel,
      context_window: contextWindow, max_output: maxOutput,
      supports_tools: input.tools, supports_vision: input.vision, supports_reasoning: input.reasoning,
      credit_multiplier: credit, visible: input.hidden !== true, sort_order: 0, aliases: [], fallback_chain: [],
      rate_multiplier: rateText ? rate : null, display_name: displayName || null, description: null,
    };
    taken.push(mapping.id);
    // First or last needs no other model to move; between two, the whole group is numbered again,
    // so a tie can never put the new model anywhere but where it was placed.
    if (place.at === 'after') {
      const anchor = place.id, index = peers.findIndex(model => model.id === anchor);
      if (index < 0) throw new Error(`要排在其后的模型不在${groupName}里，请重新选择位置`);
      const ordered = [...peers.slice(0, index + 1), mapping, ...peers.slice(index + 1)];
      mapping.sort_order = index + 1;
      moved = [...moved, ...renumbered(ordered).filter(model => model.id !== mapping.id)];
    } else if (peers.length) mapping.sort_order = place.at === 'first' ? Math.min(...peers.map(order)) - 1 : Math.max(...peers.map(order)) + 1;
    mappings.push(mapping);
  }
  // One price per price table: kept when the table already prices this model ID (unless a new one
  // is asked for, from a later time), else the table's first one, in force at once.
  const versions: Row[] = [], sharedWith: Row[] = [], ids = config.versions.map(item => item.id);
  const tables = [...new Set(mappings.map(mapping => String(config.groups.find(group => group.id === mapping.group_id)?.rate_card_id)))];
  for (const rateCardId of tables) {
    const existing = config.versions.filter(version => version.rate_card_id === rateCardId && version.model === modelId);
    sharedWith.push(...config.groups.filter(group => group.rate_card_id === rateCardId && !chosen.some(item => item.groupId === group.id)
      && config.models.some(model => model.group_id === group.id && model.exposed_model_id === modelId)));
    if (existing.length) {
      if (!currentVersion(config.versions, rateCardId, [modelId, targetModel], context.nowSecs)) {
        const next = existing.map(item => Number(item.effective_from_secs)).sort((a, b) => a - b)[0];
        throw new Error(`价格表里 ${modelId} 的价格还没生效（${new Date(next * 1000).toLocaleString()} 起），生效后再上架，或换一个模型 ID`);
      }
      if (input.keepPrice) continue;
    }
    const effectiveSecs = existing.length ? context.effectiveSecs : 0;
    const id = versionIdFor(modelId, effectiveSecs || context.nowSecs, ids);
    ids.push(id);
    let version: Row;
    if (input.official) {
      if (effectiveSecs !== 0 && (!Number.isSafeInteger(effectiveSecs) || effectiveSecs <= context.nowSecs)) throw new Error('生效时间需晚于现在，不能追溯生效');
      const problem = officialProblem(input.official);
      if (problem) throw new Error(problem);
      if (config.versions.some(item => item.rate_card_id === rateCardId && item.model === modelId && Number(item.effective_from_secs) === effectiveSecs)) throw new Error('这一时刻已有这个模型的价格版本，请稍后再上架');
      version = officialVersion(input.official, {id, rateCardId, model: modelId, effectiveSecs});
    } else {
      version = buildPriceVersion({prices: input.prices, costs: input.costs, currency: input.currency, multiplier: '1', effectiveSecs, id},
        {model: modelId, rateCardId, versions: config.versions, nowSecs: context.nowSecs, base: null});
      if (!Number(version[PRICE_FIELDS[0][0]]) && !Number(version[PRICE_FIELDS[1][0]])) throw new Error('输入和输出售价不能都是 0');
    }
    versions.push(version);
  }
  const firstTable = String(config.groups.find(group => group.id === mappings[0].group_id)?.rate_card_id);
  return {models: [...moved, ...mappings], mapping: mappings[0], mappings, version: versions.find(version => version.rate_card_id === firstTable) ?? null, versions, sharedWith};
}
