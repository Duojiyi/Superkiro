// 上架模型: the rules for listing a new model, as pure functions so they can be tested without
// a browser. A listing is one publication: the model, shown, together with its first price in
// force at once (effective_from_secs 0, which the server accepts only for a model its price
// table has no version of yet). When the table already prices that model ID — another group
// shares the table — the model is listed at that price, or with a new one from a later time.
// Either way a customer never sees a model without a price in force.
import {buildPriceVersion, currentVersion, MAX_PRICE_MICRO, PRICE_FIELDS, versionIdFor} from './priceChange';
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

/** A non-negative decimal as typed ("4", "0.24"), exactly: n / d with d a power of ten. */
function exact(text: string, what: string): {n: bigint; d: bigint} {
  const match = /^(\d{1,9})(?:\.(\d{1,9}))?$/.exec(text.trim());
  if (!match) throw new Error(`${what}须为非负数，最多 9 位小数`);
  const fraction = match[2] ?? '';
  return {n: BigInt(match[1] + fraction), d: 10n ** BigInt(fraction.length)};
}

/**
 * Credits per million tokens (as micro-credits, the way prices are stored) for an official USD
 * price sold at `retail` yuan per official dollar, a credit being worth `face` yuan:
 * official × retail ÷ face, rounded half up to one micro-credit.
 */
export function creditsFromOfficial(official: string, retail: string, face: unknown): number {
  const price = exact(official, '官方价'), rate = exact(retail, '售价倍率');
  if (typeof face !== 'number' || !Number.isFinite(face) || face <= 0) throw new Error('积分面值无效，请先在“财务对账”核对');
  const value = exact(String(face), '积分面值');
  const numerator = price.n * rate.n * 1_000_000n * value.d, denominator = price.d * rate.d * value.n;
  const micro = (2n * numerator + denominator) / (2n * denominator);
  if (micro > BigInt(MAX_PRICE_MICRO)) throw new Error('售价最多 1,000,000 积分 / 百万 Tokens');
  return Number(micro);
}

/** What the upstream charges per million tokens: official × its multiplier, in yuan (¥1 = $1). */
export function costFromOfficial(official: string, upstream: string): number {
  const price = exact(official, '官方价'), rate = exact(upstream, '成本倍率');
  const scale = 10n ** 12n, denominator = price.d * rate.d;
  const scaled = (2n * price.n * rate.n * scale + denominator) / (2n * denominator);
  const value = Number(scaled) / 1e12;
  if (value > 1_000_000) throw new Error('采购价需在 0–1,000,000 之间');
  return value;
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

export interface ListingInput {
  providerId: string;
  targetModel: string;
  /** What customers see and request. */
  modelId: string;
  /** Empty: the server derives one (displayNameFor). */
  displayName: string;
  groupId: string;
  contextWindow: number | null;
  maxOutput: number | null;
  tools: boolean;
  vision: boolean;
  reasoning: boolean;
  /** The multiplier the model list shows; empty: the server derives one from the price. */
  rateMultiplier: string;
  /** The model's own charge multiplier (扣费倍率). */
  creditMultiplier: string;
  /** Where in the group: first, last, or after the model with this entry ID. */
  place: {at: 'first' | 'last'} | {at: 'after'; id: string};
  /** Charge the price the table already has for this model ID (another group shares the table). */
  keepPrice: boolean;
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
  /** Everything to publish: the new entry, and the group's models whose place number changes. */
  models: Row[];
  mapping: Row;
  /** The new price, or null when the table's existing one is kept. */
  version: Row | null;
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
  const group = config.groups.find(item => item.id === input.groupId);
  if (!group) throw new Error('请选择分组');
  const rateCardId = group.rate_card_id;
  if (typeof rateCardId !== 'string' || !config.rate_cards.some(card => card.id === rateCardId)) throw new Error('这个分组没有价格表，不能上架');
  const peers = groupModels(config.models, group.id);
  if (peers.some(model => model.exposed_model_id === modelId || (Array.isArray(model.aliases) && model.aliases.includes(modelId)))) {
    throw new Error(`这个分组里已经有 ${modelId}：换一个模型 ID，或直接给它调价`);
  }
  const displayName = input.displayName.trim();
  if (displayName && !validText(displayName, 64)) throw new Error('显示名称不超过 64 字节，不含控制字符');
  const {contextWindow, maxOutput} = input;
  if (!Number.isSafeInteger(contextWindow) || !Number.isSafeInteger(maxOutput) || Number(maxOutput) < 1 || Number(maxOutput) > Number(contextWindow) || Number(contextWindow) > 10_000_000) {
    throw new Error('上下文须为不超过 10,000,000 的正整数 Tokens，最大输出须为正整数且不超过上下文');
  }
  const rateText = input.rateMultiplier.trim(), rate = Number(rateText);
  if (rateText && (!Number.isFinite(rate) || rate <= 0 || rate > 1000)) throw new Error('显示倍率需大于 0、不超过 1000；留空按价格自动换算');
  const creditText = input.creditMultiplier.trim(), credit = Number(creditText);
  if (!creditText || !Number.isFinite(credit) || credit <= 0 || credit > 1000) throw new Error('扣费倍率需大于 0、不超过 1000（不加倍填 1）');
  const mapping: Row = {
    id: mappingIdFor(String(provider.id), modelId, config.models.map(model => model.id)),
    group_id: group.id, exposed_model_id: modelId, target_provider_id: provider.id, target_model: targetModel,
    context_window: contextWindow, max_output: maxOutput,
    supports_tools: input.tools, supports_vision: input.vision, supports_reasoning: input.reasoning,
    credit_multiplier: credit, visible: true, sort_order: 0, aliases: [], fallback_chain: [],
    rate_multiplier: rateText ? rate : null, display_name: displayName || null, description: null,
  };
  // First or last needs no other model to move; between two, the whole group is numbered again,
  // so a tie can never put the new model anywhere but where it was placed.
  let moved: Row[] = [];
  if (input.place.at === 'after') {
    const anchor = input.place.id, index = peers.findIndex(model => model.id === anchor);
    if (index < 0) throw new Error('要排在其后的模型不在这个分组里，请重新选择位置');
    const ordered = [...peers.slice(0, index + 1), mapping, ...peers.slice(index + 1)];
    mapping.sort_order = index + 1;
    moved = renumbered(ordered).filter(model => model.id !== mapping.id);
  } else if (peers.length) mapping.sort_order = input.place.at === 'first' ? Math.min(...peers.map(order)) - 1 : Math.max(...peers.map(order)) + 1;
  const shared = sharedPrice(config, group.id, modelId);
  let version: Row | null = null;
  if (shared.versions.length) {
    if (!currentVersion(config.versions, rateCardId, [modelId, targetModel], context.nowSecs)) {
      const next = shared.versions.map(item => Number(item.effective_from_secs)).sort((a, b) => a - b)[0];
      throw new Error(`价格表里 ${modelId} 的价格还没生效（${new Date(next * 1000).toLocaleString()} 起），生效后再上架，或换一个模型 ID`);
    }
    if (!input.keepPrice) version = buildPriceVersion(
      {prices: input.prices, costs: input.costs, currency: input.currency, multiplier: '1', effectiveSecs: context.effectiveSecs,
        id: versionIdFor(modelId, context.effectiveSecs, config.versions.map(item => item.id))},
      {model: modelId, rateCardId, versions: config.versions, nowSecs: context.nowSecs, base: null});
  } else {
    version = buildPriceVersion(
      {prices: input.prices, costs: input.costs, currency: input.currency, multiplier: '1', effectiveSecs: 0,
        id: versionIdFor(modelId, context.nowSecs, config.versions.map(item => item.id))},
      {model: modelId, rateCardId, versions: config.versions, nowSecs: context.nowSecs, base: null});
  }
  if (version && !Number(version[PRICE_FIELDS[0][0]]) && !Number(version[PRICE_FIELDS[1][0]])) throw new Error('输入和输出售价不能都是 0');
  return {models: [...moved, mapping], mapping, version, sharedWith: shared.groups};
}
