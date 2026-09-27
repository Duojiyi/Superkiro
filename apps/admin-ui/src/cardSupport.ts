// What the support actions on a card (解封, 解绑设备, 重置换绑次数, 延长有效期, 备注, 换分组) mean,
// in the console's words: the device allowance, what an extension will make of a card's validity
// (as crates/billing/src/engine.rs extend_validity computes it), what the card's history records,
// and why the server refused one. Pure functions, so the rules can be tested without a browser.
import {formatFullDateTime, shortId} from './format';
import {explainRefusal} from './refusal';

/** A card as far as these rules read it (the fields of AdminCardItem they need). */
export interface SupportCard {
  id: string;
  status: string;
  archivedAt?: number | null;
  activatedAt?: number | null;
  validUntil?: number | null;
  activationDurationSecs?: number | null;
  rebindsUsed?: number;
  maxRebinds?: number;
  rebindCooldownUntil?: number | null;
}

const DAY = 86400;
/** The validity a card issued without one gets at activation (billing's LEGACY_ACTIVATION_SECS). */
const LEGACY_ACTIVATION_SECS = 30 * DAY;
/** Choices for 延长有效期, in days. */
export const EXTENSION_DAYS = [1, 3, 7, 30];
/** A reason the card's history keeps: 1–200 bytes once trimmed. 60 characters always fit. */
export const REASON_MAX_CHARS = 60;
/** Most cards one 延长有效期 may name; the server extends all of them or none. */
export const MAX_EXTEND_CARDS = 500;
/** An unconfirmed support action, kept in this tab until the list has been checked. */
export const CARD_CHANGE_KEY = 'admin-pending-card-change:v1';

const bytes = (text: string) => new TextEncoder().encode(text).length;

/** A reason the server takes: 1–200 bytes once trimmed. */
export const validReason = (text: string) => !!text.trim() && bytes(text.trim()) <= 200;

/** What is wrong with a note, or '' (at most 256 bytes once trimmed, no control characters; empty clears it). */
export function noteProblem(text: string): string {
  const note = text.trim();
  if (/[\x00-\x1f\x7f-\x9f]/.test(note)) return '备注不能包含换行等控制字符';
  return bytes(note) > 256 ? `备注最多 256 字节（现在 ${bytes(note)} 字节，约 85 个汉字）` : '';
}

/** Time left in words: 5 小时, 40 分钟. */
function left(secs: number): string {
  return secs >= 3600 ? `${Math.ceil(secs / 3600)} 小时` : `${Math.max(1, Math.ceil(secs / 60))} 分钟`;
}

/** The customer's own unbindings: 已换绑 2/5 次 · 冷却剩 5 小时. Null when the server does not report them. */
export function rebindText(card: SupportCard, nowSecs: number): string | null {
  if (typeof card.maxRebinds !== 'number' || typeof card.rebindsUsed !== 'number') return null;
  const until = Number(card.rebindCooldownUntil);
  return `已换绑 ${card.rebindsUsed}/${card.maxRebinds} 次${until > nowSecs ? ` · 冷却剩 ${left(until - nowSecs)}` : ''}`;
}

/** Whether 重置换绑次数 would change anything: unbindings used, or a cooldown running. */
export const rebindsToReset = (card: SupportCard, nowSecs: number) =>
  (card.rebindsUsed ?? 0) > 0 || Number(card.rebindCooldownUntil) > nowSecs;

/** Not yet activated (or frozen or banned before it was): its validity still counts from activation. */
export const awaitsActivation = (card: SupportCard) =>
  card.activatedAt == null && card.validUntil == null && !['active', 'expired'].includes(card.status);

export type Extension =
  | {kind: 'until'; from: number; to: number}
  | {kind: 'duration'; fromSecs: number; toSecs: number}
  | {kind: 'refused'; reason: string};

/**
 * What 延长有效期 makes of a card, as the server computes it: an activated card's expiry moves on
 * by the days given, from when it ends or from now once it has ended, or to the time given (never
 * earlier than it is); a card not yet activated keeps its validity from activation, that many days
 * longer. Refused for voided and archived cards, cards that never expire, and a time for a card
 * not yet activated or earlier than its expiry.
 */
export function extension(card: SupportCard, change: {days: number} | {validUntilSecs: number}, nowSecs: number): Extension {
  if (card.status === 'voided') return {kind: 'refused', reason: '已作废'};
  if (card.archivedAt != null) return {kind: 'refused', reason: '已归档'};
  const waiting = awaitsActivation(card);
  if (waiting ? card.activationDurationSecs === 0 : card.validUntil == null) return {kind: 'refused', reason: '永不过期'};
  if ('days' in change) {
    if (waiting) {
      const fromSecs = card.activationDurationSecs ?? LEGACY_ACTIVATION_SECS;
      return {kind: 'duration', fromSecs, toSecs: fromSecs + change.days * DAY};
    }
    const from = Number(card.validUntil);
    return {kind: 'until', from, to: Math.max(from, Math.floor(nowSecs)) + change.days * DAY};
  }
  if (waiting) return {kind: 'refused', reason: '未激活，只能按天数延长'};
  const from = Number(card.validUntil);
  if (from > change.validUntilSecs) return {kind: 'refused', reason: '现在的到期时间更晚'};
  return {kind: 'until', from, to: change.validUntilSecs};
}

/** The end of a local day (23:59:59) chosen in a date input (2026-10-05), in seconds; null when not a date. */
export function endOfDay(date: string): number | null {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(date)) return null;
  const end = new Date(`${date}T23:59:59`).getTime();
  return Number.isFinite(end) ? Math.floor(end / 1000) : null;
}

/** 2026-10-05 02:30 */
export const minuteText = (secs: number) => formatFullDateTime(secs).slice(0, 16);

/** A validity in whole days, for 激活后 N 天. */
export const daysText = (secs: number) => `${Math.round(secs / DAY)} 天`;

/** What an extension does to one card, in a line: 到期 09-28 02:30 → 10-05 02:30. */
export function extensionText(result: Extension): string {
  if (result.kind === 'refused') return `不能延期：${result.reason}`;
  if (result.kind === 'duration') return `激活后有效 ${daysText(result.fromSecs)} → ${daysText(result.toSecs)}`;
  return `到期 ${minuteText(result.from)} → ${minuteText(result.to)}`;
}

const HISTORY_LABEL: Record<string, string> = {
  issued: '发卡', activated: '激活', topup: '充值', adjust: '调账', freeze: '冻结', unfreeze: '解冻',
  ban: '封禁', unban: '解封', void: '永久作废', archive: '归档', unarchive: '取消归档',
  unbind: '解绑设备', rebinds_reset: '重置换绑次数', extend: '延长有效期', note: '修改备注', group: '换分组',
};

/** A history entry's action in words; unknown ones are shown as they are. */
export const historyLabel = (action: string) => HISTORY_LABEL[action] ?? action;

/** What a history entry recorded besides who and why: the device, the new expiry, the groups. */
export function historyDetail(event: {action: string; detail?: unknown}, groupName: (id: string) => string = id => id): string {
  const detail = event.detail && typeof event.detail === 'object' ? event.detail as Record<string, unknown> : {};
  const number = (value: unknown) => typeof value === 'number' && Number.isFinite(value) ? value : null;
  switch (event.action) {
    case 'unbind': return typeof detail.deviceId === 'string' ? `设备 ${shortId(detail.deviceId, 'device')}` : '';
    case 'rebinds_reset': {
      const used = number(detail.previousRebinds), until = number(detail.previousCooldownUntil);
      return used === null ? '' : `原已换绑 ${used} 次${until ? `，冷却到 ${minuteText(until)}` : ''}`;
    }
    case 'extend': {
      const until = number(detail.validUntil), duration = number(detail.activationDurationSecs);
      return until ? `到期改为 ${minuteText(until)}` : duration ? `激活后有效 ${daysText(duration)}` : '';
    }
    case 'group': return typeof detail.groupId === 'string'
      ? `${typeof detail.previousGroupId === 'string' ? groupName(detail.previousGroupId) : '—'} → ${groupName(detail.groupId)}` : '';
    default: return '';
  }
}

/** Cards named in a refusal: the first few short IDs, then how many there are. */
function cardList(text: string): string {
  const ids = text.split(/[\s,，、]+/).map(id => id.trim()).filter(Boolean);
  return `${ids.slice(0, 6).map(id => shortId(id, 'card')).join('、')}${ids.length > 6 ? ` 等 ${ids.length} 张` : ''}`;
}

const STATUS_NAME: Record<string, string> = {Active: '使用中', Unactivated: '未激活', Frozen: '已冻结', Banned: '已封禁', Voided: '已作废', Expired: '已到期'};

/** The server's words for a refused support action (after any "Invalid …:" prefix), and what they mean here. */
const CARD_REFUSALS: Array<[RegExp, (rest: string, match: RegExpExecArray) => string]> = [
  [/A reason of 1 to 200 bytes is required/i, () => '请填写原因（1–200 字节，约 60 个汉字）'],
  [/cannot unban (\w+)/i, (_, match) => `只有已封禁的卡可以解封（这张卡现在${STATUS_NAME[match[1]] ? `是${STATUS_NAME[match[1]]}` : '不是已封禁'}）`],
  [/Archived cards must be unarchived before they are unbanned/i, () => '已归档的卡要先取消归档，再解封'],
  [/Device .+ not found for card/i, () => '这台设备已经不在这张卡上（可能刚被解绑或换绑）：请刷新'],
  [/Card (.+) not found/i, (_, match) => match[1].includes(',') ? `没有这些卡密：${cardList(match[1])}，请刷新` : '没有这张卡密（可能刚被删除），请刷新'],
  [/Unknown group/i, rest => `分组不存在${rest ? `：${rest}` : ''}（可能刚被删除），请刷新`],
  [/Group does not take cards/i, rest => `这个分组不接收卡密（没有开启“可发新卡”）${rest ? `：${rest}` : ''}`],
  [/Voided cards cannot be extended/i, rest => `已作废的卡不能延期：${cardList(rest)}`],
  [/Archived cards must be unarchived before they are extended/i, rest => `已归档的卡要先取消归档，再延期：${cardList(rest)}`],
  [/Cards that never expire cannot be extended/i, rest => `永不过期的卡不用延期：${cardList(rest)}`],
  [/An expiry date applies only to activated cards/i, rest => `还没激活的卡只能按天数延长（从激活起算）：${cardList(rest)}`],
  [/The new expiry is earlier than the current one of/i, rest => `新的到期时间早于这些卡现在的到期时间：${cardList(rest)}`],
  [/card (\S+) cannot be extended/i, (_, match) => `这张卡不能延期：${shortId(match[1], 'card')}`],
  [/cardIds must name 1 to 500 cards/i, () => `一次只能延长 1–${MAX_EXTEND_CARDS} 张卡`],
  [/days must be between 1 and 3650|extension must be at least one day/i, () => '延长的天数须在 1–3650 天之间'],
  [/validUntilSecs must be in the future and within 3650 days|the new expiry must be in the future/i, () => '新的到期时间须晚于现在，且在 3650 天以内'],
  [/Give exactly one of days and validUntilSecs/i, () => '请选择延长的天数或到期日期（二选一）'],
  [/note must be at most 256 bytes/i, () => '备注最多 256 字节（约 85 个汉字），不能包含换行等控制字符'],
  [/The change could not be saved/i, () => '服务器没能保存这次修改，什么都没有改：请稍后重试'],
  [/cardId and deviceId are required|cardId and groupId are required|cardId is required|Invalid card ID in cardIds/i, () => '请求缺少卡密、设备或分组，请刷新后重试'],
  [/Invalid request body/i, () => '提交的内容无效，请刷新后重试'],
  [/invocationId must be/i, () => '关联的请求编号无效（最多 257 个字母、数字或 - _ . :）'],
  [/Operator ID is required/i, () => '需要重新登录以确认操作人'],
];

/** A refused support action in words the owner can act on; anything else goes to the general explanations. */
export function explainCardRefusal(text: string): string {
  for (const [pattern, explain] of CARD_REFUSALS) {
    const match = pattern.exec(text);
    if (!match) continue;
    return explain(text.slice(match.index + match[0].length).replace(/^[\s:：]+/, '').trim(), match);
  }
  return explainRefusal(text);
}

const statusOf = (error: unknown) => {
  const status = error && typeof error === 'object' ? (error as {status?: unknown}).status : undefined;
  return typeof status === 'number' ? status : 0;
};

/**
 * Whether a failed support action certainly changed nothing: the server refused it (400, 403,
 * 404, 409, 413, 422), or said it could not save it (503 in its words). Anything else, a timeout
 * or a lost reply, may have been applied and must be checked first.
 */
export function unchanged(error: unknown): boolean {
  const status = statusOf(error), text = error instanceof Error ? error.message : String(error);
  return [400, 403, 404, 409, 413, 422].includes(status) || (status === 503 && /could not be saved/i.test(text));
}
