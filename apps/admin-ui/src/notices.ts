// 公告: when an announcement is shown (from its start to its end), to whom (every customer, or the
// cards of some groups), where it stands and who edited it, and the server's refusals, in the
// console's words. Pure functions, so the rules can be tested without a browser.
import type {AdminAnnouncement} from './api';
import {minuteStart} from './traceQuery';

/** What the customer's client calls each level (desktop-ui Announcements). */
export const CLIENT_LEVEL: Record<string, string> = {info: '通知', warning: '重要', critical: '紧急'};
export const NOTICE_DAYS = [1, 3, 7, 30];
/** How far ahead a start or an end may be: ten years. */
export const MAX_AHEAD_SECS = 3650 * 86400;
export const MAX_AUDIENCE = 50;
/** Who sees an announcement for some groups: the server shows it only to a caller signed in with such a card. */
export const AUDIENCE_NOTE = '只有登录着这些分组的卡的客户端才看得到。目前发布的客户端版本拉取公告时不带卡的登录信息，分组公告要等新版客户端发布后才会送达；没有登录的客户端只看到面向全部客户的公告。';

export type NoticeStatus = 'scheduled' | 'active' | 'ended' | 'withdrawn';
export const NOTICE_STATUS: Record<NoticeStatus, {label: string; tone: 'info' | 'success' | 'outline' | 'neutral'}> = {
  scheduled: {label: '未开始', tone: 'info'}, active: {label: '生效中', tone: 'success'}, ended: {label: '已结束', tone: 'outline'}, withdrawn: {label: '已撤回', tone: 'neutral'},
};

/** Where it stands: the server's status, else as an older server's fields imply (it has no start). */
export function noticeStatus(notice: Pick<AdminAnnouncement, 'status' | 'enabled' | 'expires_at' | 'starts_at'>, nowSecs: number): NoticeStatus {
  if (notice.status && notice.status in NOTICE_STATUS) return notice.status;
  if (!notice.enabled) return 'withdrawn';
  if (notice.expires_at && nowSecs >= notice.expires_at) return 'ended';
  if (notice.starts_at && nowSecs < notice.starts_at) return 'scheduled';
  return 'active';
}

/** When a draft is shown: from now or a minute, to so many days after its start, a minute, or until withdrawn. */
export interface NoticeTiming {start: 'now' | 'at'; startAt: string; end: 'days' | 'at' | 'never'; days: number; endAt: string}

/**
 * The window a draft asks the server for: startsAtSecs (left out for now, and for a start already
 * past, which the server takes as now), and ttlSecs (days from the start), endsAtSecs, or neither
 * (shown until withdrawn); or what is wrong with it, by the server's rules.
 */
export interface NoticeWindow {startsAtSecs?: number; ttlSecs?: number; endsAtSecs?: number; error?: string}
export function noticeWindow(timing: NoticeTiming, nowSecs: number): NoticeWindow {
  const now = Math.floor(nowSecs);
  let start = now, startsAtSecs: number | undefined;
  if (timing.start === 'at') {
    const at = minuteStart(timing.startAt);
    if (at === null) return {error: '请填写开始时间'};
    if (at > now + MAX_AHEAD_SECS) return {error: '开始时间要在 3650 天以内'};
    if (at > now) {start = at; startsAtSecs = at;}
  }
  if (timing.end === 'days') return {...(startsAtSecs ? {startsAtSecs} : {}), ttlSecs: timing.days * 86400};
  if (timing.end === 'never') return startsAtSecs ? {startsAtSecs} : {};
  const end = minuteStart(timing.endAt);
  if (end === null) return {error: '请填写结束时间'};
  if (end <= start) return {error: '结束时间要晚于开始时间'};
  if (end > now + MAX_AHEAD_SECS) return {error: '结束时间要在 3650 天以内'};
  return {...(startsAtSecs ? {startsAtSecs} : {}), endsAtSecs: end};
}

const pad = (value: number) => String(value).padStart(2, '0');
/** 09-28 02:00, or 2027-01-05 02:00 in another year. */
export function momentText(secs: number, nowSecs: number): string {
  const date = new Date(secs * 1000), clock = `${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
  return date.getFullYear() === new Date(nowSecs * 1000).getFullYear() ? clock : `${date.getFullYear()}-${clock}`;
}

/** A window in words: 立即开始 · 7 天后结束; 09-28 02:00 开始 · 09-28 06:00 结束; … · 一直显示，直到撤回. */
export function windowText(window: {startsAtSecs?: number; ttlSecs?: number; endsAtSecs?: number}, nowSecs: number): string {
  const start = window.startsAtSecs ? `${momentText(window.startsAtSecs, nowSecs)} 开始` : '立即开始';
  const end = window.ttlSecs ? `${window.ttlSecs / 86400} 天后结束` : window.endsAtSecs ? `${momentText(window.endsAtSecs, nowSecs)} 结束` : '一直显示，直到撤回';
  return `${start} · ${end}`;
}

/** Who is shown it: 全部客户, or the groups by name. */
export const audienceText = (audience: string[] | undefined, groupName: (id: string) => string = id => id) =>
  audience?.length ? audience.map(groupName).join('、') : '全部客户';
/** The same as a sentence: 对全部客户可见, 只对 PRO+、PRO Max 分组的卡可见. */
export const reachText = (audience: string[] | undefined, groupName: (id: string) => string = id => id) =>
  audience?.length ? `只对 ${audienceText(audience, groupName)} 分组的卡可见` : '对全部客户可见';

const CHANGED: Record<string, string> = {title: '标题', content: '正文', level: '等级', starts_at: '开始时间', expires_at: '结束时间', audience: '对象'};
/** An edit in words: admin 09-27 15:02 改了标题、开始时间. */
export const editText = (edit: {operator: string; at_secs: number; changed: string[]}, nowSecs: number) =>
  `${edit.operator} ${momentText(edit.at_secs, nowSecs)} 改了${edit.changed.map(field => CHANGED[field] ?? field).join('、')}`;

/** The fields an edit sends: only those that differ from the announcement (null ends it never). */
export function noticeEdit(notice: AdminAnnouncement, next: {title: string; content: string; level: AdminAnnouncement['level']; startsAtSecs?: number; endsAtSecs?: number | null; audience?: string[]}):
  Record<string, unknown> {
  const edit: Record<string, unknown> = {};
  if (next.title !== notice.title) edit.title = next.title;
  if (next.content !== notice.content) edit.content = next.content;
  if (next.level !== notice.level) edit.level = next.level;
  if (next.startsAtSecs !== undefined && next.startsAtSecs !== (notice.starts_at ?? notice.created_at)) edit.startsAtSecs = next.startsAtSecs;
  if (next.endsAtSecs !== undefined && next.endsAtSecs !== (notice.expires_at ?? null)) edit.endsAtSecs = next.endsAtSecs;
  if (next.audience && [...next.audience].sort().join('\n') !== [...(notice.audience ?? [])].sort().join('\n')) edit.audience = next.audience;
  return edit;
}

// The server's refusals, and its answers that nothing was changed.
const REFUSALS: Array<[RegExp, (rest: string) => string]> = [
  [/announcement fields are invalid/i, () => '标题要 1–256 字，正文 1–20,000 字，有效期在 1 分钟到 31 天之间'],
  [/Give at most one of endsAtSecs and ttlSecs/i, () => '结束时间和有效天数只能选一个'],
  [/startsAtSecs must be within 3650 days/i, () => '开始时间要在 3650 天以内'],
  [/endsAtSecs must be after the start and within 3650 days/i, () => '结束时间要晚于开始时间，并在 3650 天以内'],
  [/audience names at most 50 groups/i, () => `最多选 ${MAX_AUDIENCE} 个分组`],
  [/Unknown group in audience:?/i, rest => `分组不存在（可能刚被删除），请刷新后重选${rest ? `：${rest}` : ''}`],
  [/announcement id is required/i, () => '缺少公告 ID，请刷新后重试'],
  [/title must be 1 to 256 characters and content 1 to 20000/i, () => '标题要 1–256 字，正文 1–20,000 字'],
  [/level must be info, warning or critical/i, () => '等级只能是普通、预警或紧急'],
  [/ttlSecs must be between 60 and 2678400/i, () => '有效期要在 1 分钟到 31 天之间'],
  [/announcement not found/i, () => '这条公告不存在（可能已被删除），请刷新'],
  [/A withdrawn announcement cannot be edited/i, () => '已撤回的公告不能再编辑'],
  [/The edit could not be saved; the announcement is unchanged/i, () => '没保存成功，公告没有改动，可以再试一次'],
  [/announcement could not be saved; nothing was published/i, () => '没保存成功，公告没有发布，可以再试一次'],
  [/withdrawal could not be saved; the announcement is still shown/i, () => '撤回没保存成功，公告仍在显示，可以再试一次'],
  [/Invalid request body: unknown field/i, () => '服务器不认识提交的字段：服务器可能还不支持这项修改'],
];
export function explainNotice(text: string): string {
  for (const [pattern, explain] of REFUSALS) {
    const match = pattern.exec(text);
    if (match) return explain(text.slice(match.index + match[0].length).trim());
  }
  return text;
}
/** The server's 503s that say nothing was changed: another try is safe. */
export const UNCHANGED_503 = /could not be saved; (nothing was published|the announcement is unchanged)|withdrawal could not be saved/i;
