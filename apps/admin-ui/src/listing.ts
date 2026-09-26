// 上架模型: the rules for listing a new model, as pure functions so they can be tested without
// a browser. A listing is published in two steps: the model (hidden) together with its first
// price, then — once that price is in force — the model shown at its place in the list. The
// server refuses a price that starts in the past, and a request for a model without a price in
// force would fail, so the model is never shown before its price applies.
import {buildPriceVersion, MAX_PRICE_MICRO, PRICE_FIELDS, versionIdFor} from './priceChange';

type Row = Record<string, unknown>;

/** Seconds from publishing a listing to its price taking effect (and the model being shown). */
export const LISTING_DELAY_SECS = 20;

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

/** The Key permissions that let a Key call this upstream model (an old Key without a list may call any). */
const keyAllows = (key: Row, model: string) => !Array.isArray(key.allowed_models) || key.allowed_models.map(String).includes(model);

/** Upstream models an enabled Key of this provider is authorised for, sorted. */
export function authorizedModels(providerId: unknown, keys: Row[]): string[] {
  return [...new Set(keys.filter(key => key.provider_id === providerId && key.enabled !== false && Array.isArray(key.allowed_models))
    .flatMap(key => (key.allowed_models as unknown[]).map(String)))].sort();
}

/** Whether an enabled Key of this provider may call this upstream model (what showing it requires). */
export function canRoute(providerId: unknown, model: string, keys: Row[]): boolean {
  return keys.some(key => key.provider_id === providerId && key.enabled !== false && keyAllows(key, model));
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

/** A group's models in list order. */
export function groupModels(models: Row[], groupId: unknown): Row[] {
  return models.filter(model => model.group_id === groupId).sort((a, b) => order(a) - order(b));
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
  /** ID of the model to place it after; empty: at the end. */
  after: string;
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
  effectiveSecs: number;
}

/**
 * Checks the form and builds the first publication: the model, hidden, and its first price.
 * Throws with the reason, in the words the drawer shows.
 */
export function buildListing(input: ListingInput, context: ListingContext): {mapping: Row; version: Row} {
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
  if (!validText(modelId, 128)) throw new Error('请填写模型 ID（客户看到的，不超过 128 字节）');
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
  let position = peers.length ? order(peers[peers.length - 1]) + 1 : 0;
  if (input.after) {
    const anchor = peers.find(model => model.id === input.after);
    if (!anchor) throw new Error('要排在其后的模型不在这个分组里，请重新选择位置');
    position = order(anchor) + 1;
  }
  const version = buildPriceVersion(
    {prices: input.prices, costs: input.costs, currency: input.currency, multiplier: '1', effectiveSecs: context.effectiveSecs,
      id: versionIdFor(modelId, context.effectiveSecs, config.versions.map(item => item.id))},
    {model: modelId, rateCardId, versions: config.versions, nowSecs: context.nowSecs, base: null},
  );
  if (!Number(version[PRICE_FIELDS[0][0]]) && !Number(version[PRICE_FIELDS[1][0]])) throw new Error('输入和输出售价不能都是 0');
  const mapping: Row = {
    id: mappingIdFor(String(provider.id), modelId, config.models.map(model => model.id)),
    group_id: group.id, exposed_model_id: modelId, target_provider_id: provider.id, target_model: targetModel,
    context_window: contextWindow, max_output: maxOutput,
    supports_tools: input.tools, supports_vision: input.vision, supports_reasoning: input.reasoning,
    credit_multiplier: 1, visible: false, sort_order: position, aliases: [], fallback_chain: [],
    rate_multiplier: rateText ? rate : null, display_name: displayName || null, description: null,
  };
  return {mapping, version};
}

/**
 * The second publication: the model as the server holds it, now shown, and the models from its
 * place on moved down one.
 */
export function showListing(models: Row[], mappingId: string): Row[] {
  const listed = models.find(model => model.id === mappingId);
  if (!listed) throw new Error('服务器上没有找到刚上架的模型，请重新加载核对');
  const shifted = models.filter(model => model.group_id === listed.group_id && model.id !== listed.id && order(model) >= order(listed))
    .map(model => ({...model, sort_order: order(model) + 1}));
  return [...shifted, {...listed, visible: true}];
}
