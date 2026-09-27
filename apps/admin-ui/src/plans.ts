// 套餐: the plan catalog cards are issued from (the commercial configuration's `plans`): what a
// card sells for and gives, its default group, and what the customer's client shows. The server
// bounds every field (billing template.rs Plan::problem); the same bounds are checked here before
// publishing. Pure functions, so the rules can be tested without a browser.
import type {Plan} from './api';

/** What Kiro accepts as a subscription type, and what it shows for each. */
export const KIRO_PLAN_TYPES = ['PRO', 'PRO_PLUS', 'PRO_MAX', 'POWER', 'CUSTOM'] as const;
export const KIRO_LABEL: Record<string, string> = {PRO: 'Kiro Pro', PRO_PLUS: 'Kiro Pro+', PRO_MAX: 'Kiro Pro Max', POWER: 'Kiro Power', CUSTOM: '自定义档位'};
export const kiroLabel = (type: unknown) => KIRO_LABEL[String(type)] ?? String(type ?? '—');
/** Most plans a catalog holds. */
export const MAX_PLANS = 100;
/** A plan publication whose result was not confirmed: editing waits until the list is checked. */
export const PLAN_CHANGE_KEY = 'admin-pending-plan-change:v1';
/** A card binds one device: the server issues no card from a plan of more. */
export const MULTI_DEVICE_NOTE = '每张卡只能绑定 1 台设备：设备数大于 1 的套餐可以保存，但还不能发卡';

// The tiers cards were issued from before the catalog (template.rs PLAN_PRICES and seed_plans).
const TIERS: Array<[string, string, number, number, string, number]> = [
  ['tier-1000', 'PRO', 1000, 30, 'PRO', 10], ['tier-2000', 'PRO+', 2000, 55, 'PRO_PLUS', 20],
  ['tier-5000', 'PRO Max', 5000, 130, 'PRO_MAX', 30], ['tier-10000', 'Power', 10000, 250, 'POWER', 40],
];

/** The plans a server that stores none issues: the four tiers, 30 days, one device, two requests at once, into group-pro-plus (or the first group by ID). */
export function seedPlans(groupIds: string[]): Plan[] {
  const group = groupIds.includes('group-pro-plus') ? 'group-pro-plus' : [...groupIds].sort()[0] ?? 'group-pro-plus';
  return TIERS.map(([id, name, points, price, kind, order]) => ({id, name, points, price_cny: price, validity_days: 30, max_devices: 1, concurrency: 2,
    default_group_id: group, kiro_plan_type: kind, on_sale: true, sort_order: order}));
}

/** The catalog in force, in its order (sort order, then ID); the seed when the server sends none. */
export function planCatalog(plans: Plan[] | null | undefined, groupIds: string[]): Plan[] {
  return [...(plans ?? seedPlans(groupIds))].sort((a, b) => a.sort_order - b.sort_order || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
}

/** Whether cards can be issued from it now: on sale, and for one device. */
export const issuable = (plan: Plan) => plan.on_sale && plan.max_devices === 1;

/** Its price in micro-CNY, as the server rounds it. */
export const priceMicro = (plan: Pick<Plan, 'price_cny'>) => Math.round(plan.price_cny * 1_000_000);

/** What the customer's client shows for a card of it. */
export function customerView(plan: Pick<Plan, 'name' | 'kiro_plan_type' | 'points' | 'validity_days' | 'concurrency'>): string {
  return `套餐「${plan.name}」· ${kiroLabel(plan.kiro_plan_type)} · ${plan.points.toLocaleString('en-US')} 积分 · 激活后 ${plan.validity_days} 天有效 · 同时最多 ${plan.concurrency} 个请求`;
}

/** The editor's fields as typed. */
export interface PlanDraft {
  id: string; name: string; points: string; price: string; validityDays: string; maxDevices: string; concurrency: string;
  defaultGroupId: string; kiroPlanType: string; onSale: boolean; sortOrder: string;
}

export const draftOf = (plan: Plan): PlanDraft => ({id: plan.id, name: plan.name, points: String(plan.points), price: String(plan.price_cny),
  validityDays: String(plan.validity_days), maxDevices: String(plan.max_devices), concurrency: String(plan.concurrency),
  defaultGroupId: plan.default_group_id, kiroPlanType: plan.kiro_plan_type, onSale: plan.on_sale, sortOrder: String(plan.sort_order)});

/** A new plan's fields: after the last in the list, into the given group. */
export const newDraft = (plans: Plan[], groupId: string): PlanDraft => ({id: '', name: '', points: '', price: '', validityDays: '30', maxDevices: '1', concurrency: '2',
  defaultGroupId: groupId, kiroPlanType: 'CUSTOM', onSale: true, sortOrder: String(plans.reduce((last, plan) => Math.max(last, plan.sort_order), 0) + 10)});

const bytes = (text: string) => new TextEncoder().encode(text).length;
const whole = (text: string, min: number, max: number) => /^-?\d+$/.test(text.trim()) && Number(text) >= min && Number(text) <= max;

/**
 * The plan the fields describe, or what is wrong with each, by the server's bounds: ID 1–64 of
 * a-z, 0-9 and - (a new one not already taken), name 1–32 bytes, 1–10,000,000 points, 0–100,000
 * yuan to the fen, 1–3650 days, 1–10 devices, 1–20 at once, a group the configuration has.
 */
export function parsePlan(draft: PlanDraft, {groupIds, takenIds = []}: {groupIds: string[]; takenIds?: string[]}):
  {plan: Plan; errors?: undefined} | {plan?: undefined; errors: Partial<Record<keyof PlanDraft, string>>} {
  const errors: Partial<Record<keyof PlanDraft, string>> = {};
  const id = draft.id.trim(), name = draft.name.trim(), price = draft.price.trim();
  if (!/^[a-z0-9-]{1,64}$/.test(id)) errors.id = '只能用 1–64 个小写字母、数字和 -（例：trial-7d）';
  else if (takenIds.includes(id)) errors.id = '已经有这个 ID 的套餐';
  if (!name || bytes(name) > 32 || /[\u0000-\u001f\u007f-\u009f]/.test(name)) errors.name = '1–32 字节（约 10 个汉字），不能有换行';
  if (!whole(draft.points, 1, 10_000_000)) errors.points = '1–10,000,000 的整数';
  if (!/^\d+(\.\d{1,2})?$/.test(price) || Number(price) > 100_000) errors.price = '0–100,000 元，最多两位小数';
  if (!whole(draft.validityDays, 1, 3650)) errors.validityDays = '1–3650 的整数';
  if (!whole(draft.maxDevices, 1, 10)) errors.maxDevices = '1–10 的整数';
  if (!whole(draft.concurrency, 1, 20)) errors.concurrency = '1–20 的整数';
  if (!groupIds.includes(draft.defaultGroupId)) errors.defaultGroupId = '请选择分组';
  if (!(KIRO_PLAN_TYPES as readonly string[]).includes(draft.kiroPlanType)) errors.kiroPlanType = '请选择';
  if (!whole(draft.sortOrder, -2147483648, 2147483647)) errors.sortOrder = '整数，小的排在前面';
  if (Object.keys(errors).length) return {errors};
  return {plan: {id, name, points: Number(draft.points), price_cny: Number(price), validity_days: Number(draft.validityDays), max_devices: Number(draft.maxDevices),
    concurrency: Number(draft.concurrency), default_group_id: draft.defaultGroupId, kiro_plan_type: draft.kiroPlanType, on_sale: draft.onSale, sort_order: Number(draft.sortOrder)}};
}

/** What a publication changes of a plan, in words: 售价 ¥55.00 → ¥59.00. */
export function planChanges(before: Plan | undefined, after: Plan, groupName: (id: string) => string = id => id): string[] {
  if (!before) return [`新套餐：${customerView(after)}`, `售价 ¥${after.price_cny.toFixed(2)} · 每张 ${after.max_devices} 台设备 · 默认分组 ${groupName(after.default_group_id)}`];
  const fields: Array<[string, (plan: Plan) => string]> = [['名称', plan => plan.name], ['积分', plan => plan.points.toLocaleString('en-US')], ['售价', plan => `¥${plan.price_cny.toFixed(2)}`],
    ['有效期', plan => `${plan.validity_days} 天`], ['设备', plan => `${plan.max_devices} 台`], ['并发', plan => `${plan.concurrency}`], ['默认分组', plan => groupName(plan.default_group_id)],
    ['Kiro 显示', plan => kiroLabel(plan.kiro_plan_type)], ['在售', plan => (plan.on_sale ? '在售' : '已下架')], ['排序', plan => String(plan.sort_order)]];
  return fields.filter(([, read]) => read(before) !== read(after)).map(([label, read]) => `${label}：${read(before)} → ${read(after)}`);
}
