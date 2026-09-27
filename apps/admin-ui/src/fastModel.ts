// Kiro's hidden background calls — commit messages, one sub-intent per spec-mode message,
// "Analyze Requirements" — ask for the model `simple-task`. The server sends them, per group, to
// the model given that alias (listed or hidden), else to the listed model whose million input and
// million output tokens cost least at the price in force (the first in the group's order among
// equals), else to the group's default; each call is billed at that model's price. The stats name
// the choice; the rules below give the same answer for a draft, and set the alias on an entry.
// Pure functions, so the rules can be tested without a browser.
import {currentVersion, PRICE_FIELDS} from './priceChange';
import {groupModels} from './listing';

type Row = Record<string, unknown>;

export const FAST_ALIAS = 'simple-task';
export type FastVia = 'alias' | 'cheapest' | 'default';
export interface FastModel {groupId: string; groupName: string; model: string | null; via: FastVia | null}

/** Why the calls go where they go, in the words the pages use. */
export const VIA_TEXT: Record<FastVia, string> = {alias: '设了别名 simple-task', cheapest: '分组里最便宜的在售模型', default: '分组的默认模型（没有能比价的在售模型）'};
export const FAST_NOTE = 'Kiro 的后台调用（提交信息、Spec 的子任务、Analyze Requirements）不经客户选择，每次按这个模型的价格扣费';

const finite = (value: unknown): value is number => typeof value === 'number' && Number.isFinite(value);

/** The stats' list, read defensively: null from a server that does not send it. */
export function fastModelsOf(value: unknown): FastModel[] | null {
  if (!Array.isArray(value)) return null;
  return value.filter((item): item is Row => !!item && typeof item === 'object' && typeof (item as Row).groupId === 'string').map(item => ({
    groupId: String(item.groupId), groupName: String(item.groupName ?? item.groupId),
    model: typeof item.model === 'string' && item.model ? item.model : null,
    via: item.via === 'alias' || item.via === 'cheapest' || item.via === 'default' ? item.via : null,
  }));
}

const hasAlias = (mapping: Row, alias: string) => mapping.exposed_model_id === alias || (Array.isArray(mapping.aliases) && mapping.aliases.map(String).includes(alias));
const listed = (mapping: Row) => mapping.visible !== false && mapping.retired !== true;

/**
 * What a card of `group` pays for one million input and one million output tokens of this entry
 * (micro-credits, every multiplier applied, rounded up as billing rounds), or null when no price
 * resolves: the server compares models by it.
 */
export function comparedPrice(mapping: Row, group: Row, versions: Row[], settings: Row | undefined, nowSecs: number): number | null {
  const version = currentVersion(versions, group.rate_card_id, [mapping.exposed_model_id, mapping.target_model], nowSecs);
  if (!version) return null;
  const factor = (finite(version.margin_multiplier) ? version.margin_multiplier : 1) * (finite(group.margin_multiplier) ? group.margin_multiplier : 1)
    * (finite(mapping.credit_multiplier) ? mapping.credit_multiplier : 1);
  if (version.pricing_mode === 'fixed') {
    const input = Number(version[PRICE_FIELDS[0][0]]), output = Number(version[PRICE_FIELDS[1][0]]);
    return finite(input) && finite(output) ? Math.ceil((input + output) * factor) : null;
  }
  // Cost-plus: the procurement prices in ¥ over the face value.
  const rate = version.currency === 'USD' ? Number(settings?.usd_cny_rate) : 1, face = Number(settings?.credit_face_value_cny) > 0 ? Number(settings?.credit_face_value_cny) : 0.01;
  const yuan = (Number(version.input_price_per_m) + Number(version.output_price_per_m)) * rate;
  return finite(yuan) ? Math.ceil(yuan * factor / face * 1_000_000) : null;
}

/** Where a group's background calls go under this configuration, as the server decides it. */
export function fastModelFor(models: Row[], group: Row, versions: Row[], settings: Row | undefined, nowSecs: number): {mapping: Row; via: FastVia} | null {
  const inGroup = groupModels(models, group.id);
  const aliased = inGroup.find(mapping => mapping.retired !== true && hasAlias(mapping, FAST_ALIAS));
  if (aliased) return {mapping: aliased, via: 'alias'};
  const shown = inGroup.filter(listed);
  let best: {mapping: Row; price: number} | null = null;
  for (const mapping of shown) {
    const price = comparedPrice(mapping, group, versions, settings, nowSecs);
    if (price !== null && price > 0 && (!best || price < best.price)) best = {mapping, price};
  }
  if (best) return {mapping: best.mapping, via: 'cheapest'};
  return shown[0] ? {mapping: shown[0], via: 'default'} : null;
}

/** Each group's choice, as the stats name it (the server's, when it sent them, else worked out here). */
export function fastModels(config: {groups: Row[]; models: Row[]; versions: Row[]; settings?: unknown}, nowSecs: number, reported: FastModel[] | null): FastModel[] {
  return config.groups.map(group => {
    const told = reported?.find(item => item.groupId === group.id);
    if (told) return {...told, groupName: String(group.name ?? told.groupName)};
    const choice = fastModelFor(config.models, group, config.versions, config.settings as Row | undefined, nowSecs);
    return {groupId: String(group.id), groupName: String(group.name ?? group.id), model: choice ? String(choice.mapping.exposed_model_id) : null, via: choice?.via ?? null};
  });
}

/** One group's choice in a line: "claude-haiku（分组里最便宜的在售模型）". */
export const fastText = (entry: FastModel) => entry.model ? `${entry.model}（${entry.via ? VIA_TEXT[entry.via] : '原因未知'}）` : '没有可用的模型：后台调用会失败';

/**
 * Every entry with the alias `simple-task` given to `targets` (entry IDs) or taken from them; the
 * other entries of the same groups lose it, since one group can give an alias to one model only.
 */
export function withFastAlias(models: Row[], targets: string[], on: boolean): Row[] {
  const groups = new Set(models.filter(model => targets.includes(String(model.id))).map(model => model.group_id));
  return models.map(model => {
    const aliases = Array.isArray(model.aliases) ? model.aliases.map(String) : [];
    const mine = targets.includes(String(model.id));
    if (mine && on) return aliases.includes(FAST_ALIAS) ? model : {...model, aliases: [...aliases, FAST_ALIAS]};
    if ((mine || groups.has(model.group_id)) && aliases.includes(FAST_ALIAS) && (mine || on)) return {...model, aliases: aliases.filter(alias => alias !== FAST_ALIAS)};
    return model;
  });
}
