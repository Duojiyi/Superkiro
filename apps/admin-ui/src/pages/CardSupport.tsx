// A card's support actions for 卡密资产 and its drawer: 解封, 解绑设备, 重置换绑次数, 延长有效期
// (one card, or the ticked cards and all a filter finds), 备注 and 换分组. Each is written to the
// card's history with the operator and the reason; the reply is the card as it now is. A refusal
// changed nothing and is explained where it happened; a result that did not come back may have
// been applied, so further changes wait until the list has been checked (like issuing cards).
import {useEffect, useRef, useState, type ReactNode} from 'react';
import {adminApi, type AdminCardItem, type ValidityChange} from '../api';
import {ask, confirmAction} from '../components/confirm';
import {Modal} from '../components/modal';
import {toast} from '../components/toast';
import {IdCell} from '../components/ui';
import {CARD_CHANGE_KEY, DAILY_WINDOW, DOWNLOAD_URL, endOfDay, unbanText, explainCardRefusal, EXTENSION_DAYS, extension, extensionText, limitInput, MAX_CONCURRENCY, MAX_EXTEND_CARDS, MONTHLY_WINDOW, quotaChange,
  REASON_MAX_CHARS, rebindText, rekeyHandout, unchanged, validReason, type QuotaChange} from '../cardSupport';
import {formatCount, formatCredits, shortId} from '../format';
import {cardState, cardStatusView} from '../status';
import type {Refresh, ReportError, Row, WriteGuards} from '../types';

interface Pending {label: string; cardIds: string[]; at: string}
const errorText = (error: unknown) => (error instanceof Error ? error.message : String(error));
const EXTEND_REASONS = ['服务中断补偿', '客户要求', '续费', '活动赠送'];

function loadPending(): Pending | null {
  try {
    const value = JSON.parse(sessionStorage.getItem(CARD_CHANGE_KEY) || 'null');
    return value && typeof value.label === 'string' && Array.isArray(value.cardIds) ? {label: value.label, cardIds: value.cardIds.map(String), at: String(value.at ?? '')} : null;
  } catch {return {label: '卡密修改', cardIds: [], at: ''};}
}

/** A local date for a date input: 2026-10-05. */
const dateText = (secs: number) => {const date = new Date(secs * 1000); return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(date.getDate()).padStart(2, '0')}`;};

function ExtendDialog({targets, everyResult, onSubmit, onClose}: {
  targets: AdminCardItem[];
  /** The cards are all a filter found, not only the ticked ones. */
  everyResult: boolean;
  onSubmit: (cards: AdminCardItem[], change: ValidityChange, reason: string) => Promise<string | null>;
  onClose: () => void;
}) {
  const [nowSecs] = useState(() => Date.now() / 1000);
  const [choice, setChoice] = useState<number | 'date'>(7);
  const single = targets.length === 1 ? targets[0] : null;
  const [date, setDate] = useState(() => dateText(Math.max(nowSecs, Number(single?.validUntil ?? 0)) + 7 * 86400));
  const [reason, setReason] = useState('');
  const [error, setError] = useState('');
  const [saving, setSaving] = useState(false);
  const until = endOfDay(date);
  const change: ValidityChange | null = choice === 'date' ? (until && until > nowSecs ? {validUntilSecs: until} : null) : {days: choice};
  const results = targets.map(card => ({card, result: change ? extension(card, change, nowSecs) : null}));
  const eligible = results.filter(entry => entry.result && entry.result.kind !== 'refused');
  const skipped = results.filter(entry => entry.result?.kind === 'refused');
  // Why cards are left out, by reason: 已作废 2 张 · 已归档 1 张.
  const skipReasons = [...skipped.reduce((counts, entry) => {
    const reason = entry.result!.kind === 'refused' ? entry.result!.reason : '';
    return counts.set(reason, (counts.get(reason) ?? 0) + 1);
  }, new Map<string, number>())].map(([text, count]) => `${text} ${count} 张`).join(' · ');
  const tooMany = eligible.length > MAX_EXTEND_CARDS;
  const ready = !!change && eligible.length > 0 && !tooMany && validReason(reason) && !saving;
  const submit = async () => {
    if (!ready || !change) return;
    setSaving(true); setError('');
    const failure = await onSubmit(eligible.map(entry => entry.card), change, reason.trim());
    setSaving(false);
    if (failure) setError(failure);
  };
  return <Modal label="延长有效期" onClose={onClose} busy={saving} className="dialog-form">
    <h3 className="modal-title">{single ? <>延长有效期 · <span className="mono">{shortId(single.id, 'card')}</span></> : `延长 ${formatCount(targets.length)} 张卡的有效期`}</h3>
    {!single && everyResult && <p className="muted">全部 {formatCount(targets.length)} 张筛选结果（不只本页）</p>}
    {error && <p role="alert" className="form-error">{error}</p>}
    <div className="segmented" role="radiogroup" aria-label="延长多久">
      {EXTENSION_DAYS.map(days => <button key={days} type="button" role="radio" aria-checked={choice === days} disabled={saving} onClick={() => setChoice(days)}>+{days} 天</button>)}
      <button type="button" role="radio" aria-checked={choice === 'date'} disabled={saving} onClick={() => setChoice('date')}>指定日期</button>
    </div>
    {choice === 'date' && <label className="field"><span className="field-label">新的到期日期</span>
      <input type="date" aria-label="新的到期日期" min={dateText(nowSecs + 86400)} value={date} disabled={saving} onChange={event => setDate(event.target.value)}/>
      <span className="field-hint">到期时间为这天 23:59；还没激活的卡只能按天数延长</span></label>}
    <div className="review-box" aria-label="延期结果" role="status">
      {!change ? <p>请选择今天以后的日期</p> : single ? <p>{extensionText(results[0].result!)}</p> : <>
        <p>将延长 <b>{formatCount(eligible.length)}</b> 张{choice === 'date' ? `，到期都改为 ${date} 23:59` : `，每张 +${choice} 天（已过期的从现在算起，未激活的加在激活后的有效期上）`}</p>
        {eligible.slice(0, 3).map(({card, result}) => <p key={card.id} className="muted"><span className="mono">{shortId(card.id, 'card')}</span>：{extensionText(result!)}</p>)}
        {skipped.length > 0 && <p className="is-warning">跳过 {formatCount(skipped.length)} 张（{skipReasons}）</p>}
      </>}
      {tooMany && <p className="is-danger">一次最多延长 {MAX_EXTEND_CARDS} 张，请缩小筛选范围</p>}
    </div>
    <div className="field">
      <label className="field-label" htmlFor="extend-reason">原因<span className="required-mark">（必填）</span></label>
      <input id="extend-reason" value={reason} maxLength={REASON_MAX_CHARS} placeholder="例：9/26 上游中断补偿" disabled={saving} onChange={event => setReason(event.target.value)}/>
      <div className="chips">{EXTEND_REASONS.map(text => <button key={text} type="button" className="chip" aria-pressed={reason === text} disabled={saving} onClick={() => setReason(text)}>{text}</button>)}</div>
    </div>
    <p className="muted">写进每张卡的操作记录。延长后不能缩短。</p>
    <div className="modal-actions">
      <button type="button" className="btn" disabled={saving} onClick={onClose}>取消</button>
      <button type="button" className="btn btn-primary" disabled={!ready} title={ready ? undefined : !validReason(reason) ? '填写原因后可以延长' : undefined} onClick={() => void submit()}>
        {saving ? '正在延长…' : single ? '延长' : `延长 ${formatCount(eligible.length)} 张`}</button>
    </div>
  </Modal>;
}

function GroupDialog({card, groups, groupName, onSubmit, onClose}: {
  card: AdminCardItem;
  groups: Row[];
  groupName: (id: string) => string;
  onSubmit: (groupId: string, reason: string) => Promise<string | null>;
  onClose: () => void;
}) {
  // Only a group that takes cards can have one moved into it.
  const choices = groups.filter(group => group.issuance_enabled !== false && group.id !== card.groupId);
  const [groupId, setGroupId] = useState(choices.length === 1 ? String(choices[0].id) : '');
  const [reason, setReason] = useState('');
  const [error, setError] = useState('');
  const [saving, setSaving] = useState(false);
  const ready = !!groupId && validReason(reason) && !saving;
  const submit = async () => {
    if (!ready) return;
    setSaving(true); setError('');
    const failure = await onSubmit(groupId, reason.trim());
    setSaving(false);
    if (failure) setError(failure);
  };
  return <Modal label="换分组" onClose={onClose} busy={saving} className="dialog-form">
    <h3 className="modal-title">换分组 · <span className="mono">{shortId(card.id, 'card')}</span></h3>
    {error && <p role="alert" className="form-error">{error}</p>}
    <p>现在：{groupName(card.groupId)}</p>
    <label className="field"><span className="field-label">新分组</span>
      <select aria-label="新分组" value={groupId} disabled={saving || !choices.length} onChange={event => setGroupId(event.target.value)}>
        <option value="" disabled>{choices.length ? '请选择分组' : '没有别的可发卡分组'}</option>
        {choices.map(group => <option key={String(group.id)} value={String(group.id)}>{String(group.name ?? group.id)}</option>)}
      </select></label>
    <p className="note-warning">客户需要重新登录：换分组后这张卡的登录会失效。之后的请求按新分组的模型和价格计费，进行中的请求按原来的价格结算。</p>
    <div className="field">
      <label className="field-label" htmlFor="group-reason">原因<span className="required-mark">（必填）</span></label>
      <input id="group-reason" value={reason} maxLength={REASON_MAX_CHARS} placeholder="例：客户升级套餐" disabled={saving} onChange={event => setReason(event.target.value)}/>
    </div>
    <div className="modal-actions">
      <button type="button" className="btn" disabled={saving} onClick={onClose}>取消</button>
      <button type="button" className="btn btn-primary" disabled={!ready} onClick={() => void submit()}>{saving ? '正在换分组…' : '换分组'}</button>
    </div>
  </Modal>;
}

const QUOTA_REASONS = ['客户要求', '防止滥用', '临时放宽'];

/** 修改限额: requests at once, and credits a day and over 30 days; only what changes is sent. */
function QuotaDialog({card, onSubmit, onClose}: {
  card: AdminCardItem;
  onSubmit: (change: QuotaChange, reason: string) => Promise<string | null>;
  onClose: () => void;
}) {
  const [concurrency, setConcurrency] = useState(String(card.maxConcurrency ?? ''));
  const [daily, setDaily] = useState(limitInput(card.dailyCreditLimit));
  const [monthly, setMonthly] = useState(limitInput(card.monthlyCreditLimit));
  const [reason, setReason] = useState('');
  const [error, setError] = useState('');
  const [saving, setSaving] = useState(false);
  const {change, problems, lines} = quotaChange(card, {concurrency, daily, monthly});
  const ready = !problems.length && lines.length > 0 && validReason(reason) && !saving;
  const submit = async () => {
    if (!ready) return;
    setSaving(true); setError('');
    const failure = await onSubmit(change, reason.trim());
    setSaving(false);
    if (failure) setError(failure);
  };
  return <Modal label="修改限额" onClose={onClose} busy={saving} className="dialog-form">
    <h3 className="modal-title">修改限额 · <span className="mono">{shortId(card.id, 'card')}</span></h3>
    {error && <p role="alert" className="form-error">{error}</p>}
    <div className="form-grid quota-grid">
      <label className="field"><span className="field-label">同时请求数</span>
        <input type="number" aria-label="同时请求数" min={1} max={MAX_CONCURRENCY} step={1} value={concurrency} disabled={saving} onChange={event => setConcurrency(event.target.value)}/>
        <span className="field-hint">1–{MAX_CONCURRENCY}{card.plan ? `；套餐是 ${card.plan.concurrency}` : ''}</span></label>
      <label className="field"><span className="field-label">每日积分上限</span>
        <input inputMode="decimal" aria-label="每日积分上限" placeholder="不限" value={daily} disabled={saving} onChange={event => setDaily(event.target.value)}/>
        <span className="field-hint">{DAILY_WINDOW}；留空为不限</span></label>
      <label className="field"><span className="field-label">近 30 天积分上限</span>
        <input inputMode="decimal" aria-label="近 30 天积分上限" placeholder="不限" value={monthly} disabled={saving} onChange={event => setMonthly(event.target.value)}/>
        <span className="field-hint">{MONTHLY_WINDOW}；留空为不限</span></label>
    </div>
    {problems.map(problem => <p key={problem} className="field-error">{problem}</p>)}
    <div className="review-box" aria-label="限额修改" role="status">
      {lines.length ? lines.map(line => <p key={line}>{line}</p>) : <p className="muted">没有修改</p>}
    </div>
    <div className="field">
      <label className="field-label" htmlFor="quota-reason">原因<span className="required-mark">（必填）</span></label>
      <input id="quota-reason" value={reason} maxLength={REASON_MAX_CHARS} placeholder="例：客户要求限制每日用量" disabled={saving} onChange={event => setReason(event.target.value)}/>
      <div className="chips">{QUOTA_REASONS.map(text => <button key={text} type="button" className="chip" aria-pressed={reason === text} disabled={saving} onClick={() => setReason(text)}>{text}</button>)}</div>
    </div>
    <p className="muted">马上生效：超过上限的新请求会被拒绝，进行中的请求照常结算。写进这张卡的操作记录，连同原来的值。</p>
    <div className="modal-actions">
      <button type="button" className="btn" disabled={saving} onClick={onClose}>取消</button>
      <button type="button" className="btn btn-primary" disabled={!ready} title={ready ? undefined : !lines.length ? '没有修改' : problems.length ? problems[0] : '填写原因后可以保存'} onClick={() => void submit()}>
        {saving ? '正在保存…' : '保存限额'}</button>
    </div>
  </Modal>;
}

/** The new code, shown this once, with copy and the text for the customer; closing asks whether it was kept. */
function NewCodeDialog({card, code, onClose}: {card: AdminCardItem; code: string; onClose: () => void}) {
  const [error, setError] = useState('');
  const copy = async (text: string, done: string) => {
    try {await navigator.clipboard.writeText(text); setError(''); toast.success(done);}
    catch {setError('复制失败，请手动选中复制');}
  };
  const finish = async () => {
    if (await confirmAction({title: '已保存新卡密？', consequence: card.codeRecoverable ? '之后可在列表里用“显示卡密”再看（会记录）。' : '关闭后不再显示，也不能再查看。', confirmLabel: '完成'})) onClose();
  };
  return <Modal label="新卡密" onClose={() => void finish()} className="dialog-narrow">
    <h3 className="modal-title">新卡密 · <span className="mono">{shortId(card.id, 'card')}</span></h3>
    <textarea aria-label="新卡密明文" readOnly rows={2} className="secret-block" value={code} onFocus={event => event.currentTarget.select()}/>
    <p className="note-warning">旧卡密已经停用，这张卡的登录都已退出：把新卡密发给客户，客户用它重新登录。</p>
    <p className="muted">卡密 ID 不变（<span className="mono">{card.id}</span>）：以后按新卡密搜索找不到这张卡，请用卡密 ID 或备注搜索。{card.codeRecoverable ? '之后可在列表里用“显示卡密”再看（会记录）。' : '只显示这一次。'}</p>
    {error && <p role="alert" className="form-error">{error}</p>}
    <div className="modal-actions">
      <button type="button" className="btn" title={`新卡密、套餐、余额、有效期和下载地址 ${DOWNLOAD_URL}`} onClick={() => void copy(rekeyHandout(card, code), '已复制发给客户的文本')}>复制发货文本</button>
      <button type="button" className="btn btn-primary" data-autofocus onClick={() => void copy(code, '已复制新卡密')}>复制</button>
      <button type="button" className="btn" onClick={() => void finish()}>完成</button>
    </div>
  </Modal>;
}

/** The end of a card's short ID, typed to confirm what cannot be undone (the whole ID when it is short). */
export const typedCardId = (id: string) => shortId(id, 'card').includes('…') ? id.slice(-4) : id;

export interface CardSupport {
  /** Why support actions are not available now (a result to check, another write), or undefined. */
  blockedTitle?: string;
  unban: (card: AdminCardItem) => void;
  unbind: (card: AdminCardItem, deviceId: string) => void;
  resetRebinds: (card: AdminCardItem) => void;
  /** Opens 延长有效期 for these cards (one, the ticked ones, or all a filter found). */
  extend: (cards: AdminCardItem[], everyResult?: boolean) => void;
  /** Saves a note; resolves to what went wrong, or null once saved. */
  saveNote: (card: AdminCardItem, note: string) => Promise<string | null>;
  changeGroup: (card: AdminCardItem) => void;
  /** Opens 修改限额 for the card. */
  changeQuotas: (card: AdminCardItem) => void;
  /** 更换卡密, after a strong confirmation; `preface` says why it is offered (from 解绑, say). */
  rekey: (card: AdminCardItem, preface?: string) => void;
  /** Counts the changes made here; a card's history is read again after each (a new code changes no field it shows). */
  revision: number;
  /** The recovery panel and the dialogs, rendered by the page. */
  view: ReactNode;
}

export function useCardSupport({cards, groups, groupName, guards, refresh, reportError, updateCards, blocked, onShowCards, onSecretShown}: {
  cards: AdminCardItem[];
  groups: Row[];
  groupName: (id: string) => string;
  guards: WriteGuards;
  refresh: Refresh;
  reportError: ReportError;
  updateCards: (cards: AdminCardItem[]) => void;
  /** The list is loading, could not be read, or another action is running: nothing is sent. */
  blocked: boolean;
  /** Shows this card in the list with its details open (the recovery panel's 查看这张卡). */
  onShowCards: (cardIds: string[]) => void;
  /** A new code is on screen (leaving the page asks first), or no longer. */
  onSecretShown?: (shown: boolean) => void;
}): CardSupport {
  const {writing, mounted} = guards;
  const [pending, setPending] = useState<Pending | null>(loadPending);
  const [review, setReview] = useState<'refresh' | 'review'>('refresh');
  const [checking, setChecking] = useState(false);
  const [working, setWorking] = useState(false);
  const [extending, setExtending] = useState<{cards: AdminCardItem[]; everyResult: boolean} | null>(null);
  const [grouping, setGrouping] = useState<AdminCardItem | null>(null);
  const [limiting, setLimiting] = useState<AdminCardItem | null>(null);
  const [rekeyed, setRekeyed] = useState<{card: AdminCardItem; code: string} | null>(null);
  const [revision, setRevision] = useState(0);
  useEffect(() => {onSecretShown?.(!!rekeyed);}, [rekeyed, onSecretShown]);
  const latest = useRef(cards);
  latest.current = cards;
  useEffect(() => {if (extending || grouping || limiting) reportError('');}, [extending, grouping, limiting, reportError]);

  /**
   * Sends one change. The intent is kept first, so a reload before the reply still knows a
   * result is missing. Resolves to what went wrong (a refusal: nothing changed), or null.
   */
  const send = async (label: string, cardIds: string[], request: () => Promise<{success: boolean; card?: AdminCardItem; cards?: AdminCardItem[]}>, done: string | (() => string)): Promise<string | null> => {
    if (writing.current || working) return '另一个操作还没完成，请稍候';
    if (pending) return '上次的卡密修改结果未确认，请先在列表上方核对';
    const intent: Pending = {label, cardIds: cardIds.slice(0, 500), at: new Date().toISOString()};
    try {sessionStorage.setItem(CARD_CHANGE_KEY, JSON.stringify(intent));}
    catch {return '无法保存待核对记录，没有发送修改：请检查浏览器存储';}
    writing.current = true; setWorking(true); reportError('');
    try {
      const result = await request();
      if (result.success !== true) throw new Error('服务器未确认修改结果');
      sessionStorage.removeItem(CARD_CHANGE_KEY);
      const changed = result.cards ?? (result.card ? [result.card] : []);
      if (mounted.current && changed.length) updateCards(latest.current.map(card => ({...card, ...changed.find(next => next.id === card.id)})));
      if (mounted.current) setRevision(value => value + 1);
      toast.success(typeof done === 'function' ? done() : done);
      void refresh({keepSelection: true});
      return null;
    } catch (error) {
      if (unchanged(error)) {
        try {sessionStorage.removeItem(CARD_CHANGE_KEY);} catch {/* the panel stays until it can be cleared */}
        return explainCardRefusal(errorText(error));
      }
      if (mounted.current) {setPending(intent); setReview('refresh');}
      reportError(`没收到${label}的结果（${errorText(error)}），可能已经生效。请先刷新列表核对，再继续修改卡密。`);
      return '';
    } finally {writing.current = false; if (mounted.current) setWorking(false);}
  };
  const unavailable = blocked || working;
  const blockedTitle = pending ? '上次的卡密修改结果未确认，请先在列表上方核对' : undefined;
  const refused = (verb: string) => (failure: string | null) => {if (failure) reportError(`没有${verb}：${failure}`);};

  const facts = (card: AdminCardItem) => [`余额 ${formatCredits(card.pointsAvailable)} / ${formatCredits(card.pointsTotal)} 积分 · ${cardStatusView(cardState(card, Date.now() / 1000)).label}`];
  const reasonField = (placeholder: string, suggestions: string[]) => ({label: '原因', placeholder, suggestions, maxLength: REASON_MAX_CHARS, required: true});

  const unban = async (card: AdminCardItem) => {
    if (unavailable || pending) return;
    const answer = await ask({
      title: `解封卡密 ${shortId(card.id, 'card')}？`, facts: facts(card),
      consequence: `恢复为封禁前的状态：${card.activatedAt ? '使用中' : '未激活'}，封禁前已冻结的仍是冻结（要再解冻）。仍受有效期和余额限制；封禁时退出的登录不会恢复，客户需要重新登录。`,
      confirmLabel: '解封', reason: reasonField('例：误封，已核实', ['误封', '已核实，恢复使用', '客户申诉通过']),
    });
    if (!answer.confirmed || !mounted.current) return;
    // The toast says what it became: a card banned while frozen is frozen again.
    let status: string | undefined;
    const label = shortId(card.id, 'card');
    refused('解封')(await send('解封', [card.id], async () => {const result = await adminApi.updateCardStatus(card.id, 'unban', answer.reason); status = result.newStatus; return result;},
      () => unbanText(label, status)));
  };

  const unbind = async (card: AdminCardItem, deviceId: string) => {
    if (unavailable || pending) return;
    const allowance = rebindText(card, Date.now() / 1000);
    const answer = await ask({
      title: `解绑设备 ${shortId(deviceId, 'device')}？`,
      facts: [<>卡密 <span className="mono">{shortId(card.id, 'card')}</span> · 设备 <span className="mono" title={deviceId}>{shortId(deviceId, 'device')}</span></>, ...(allowance ? [`客户自己的换绑：${allowance}`] : [])],
      consequence: '这台设备马上退出登录，客户在下一台电脑登录时会绑定那台。不占用客户的换绑次数。',
      confirmLabel: '解绑', reason: reasonField('例：客户旧电脑已损坏', ['旧电脑损坏', '客户换电脑', '客户要求']),
    });
    if (!answer.confirmed || !mounted.current) return;
    refused('解绑')(await send('解绑设备', [card.id], () => adminApi.unbindDevice(card.id, deviceId, answer.reason), `已解绑设备 ${shortId(deviceId, 'device')}`));
  };

  const resetRebinds = async (card: AdminCardItem) => {
    if (unavailable || pending) return;
    const allowance = rebindText(card, Date.now() / 1000);
    const answer = await ask({
      title: `重置 ${shortId(card.id, 'card')} 的换绑次数？`, facts: allowance ? [allowance] : undefined,
      consequence: `客户重新有 ${card.maxRebinds ?? '全部'} 次自助换绑，冷却马上结束。已绑定的设备不变。`,
      confirmLabel: '重置', reason: reasonField('例：客户多次换电脑', ['客户多次换电脑', '客户要求']),
    });
    if (!answer.confirmed || !mounted.current) return;
    refused('重置')(await send('重置换绑次数', [card.id], () => adminApi.resetRebinds(card.id, answer.reason), `已重置 ${shortId(card.id, 'card')} 的换绑次数`));
  };

  const extend = (targets: AdminCardItem[], everyResult = false) => {
    if (unavailable || pending || !targets.length) return;
    setExtending({cards: targets, everyResult});
  };
  const submitExtension = async (targets: AdminCardItem[], change: ValidityChange, reason: string) => {
    const result = await send('延长有效期', targets.map(card => card.id), () => adminApi.extendValidity(targets.map(card => card.id), change, reason),
      targets.length === 1 ? `已延长 ${shortId(targets[0].id, 'card')} 的有效期` : `已延长 ${formatCount(targets.length)} 张卡的有效期`);
    if (result === null || result === '') setExtending(null);
    return result || null;
  };

  const saveNote = async (card: AdminCardItem, note: string) => {
    if (unavailable) return '另一个操作还没完成，请稍候';
    const result = await send('修改备注', [card.id], () => adminApi.setCardNote(card.id, note.trim()), note.trim() ? '已保存备注' : '已清除备注');
    return result === '' ? null : result;
  };

  const changeGroup = (card: AdminCardItem) => {if (!unavailable && !pending) setGrouping(card);};
  const submitGroup = async (card: AdminCardItem, groupId: string, reason: string) => {
    const result = await send('换分组', [card.id], () => adminApi.changeCardGroup(card.id, groupId, reason), `已把 ${shortId(card.id, 'card')} 换到 ${groupName(groupId)}`);
    if (result === null || result === '') setGrouping(null);
    return result || null;
  };

  const changeQuotas = (card: AdminCardItem) => {if (!unavailable && !pending) setLimiting(card);};
  const submitQuotas = async (card: AdminCardItem, change: QuotaChange, reason: string) => {
    const result = await send('修改限额', [card.id], () => adminApi.changeCardQuotas(card.id, change, reason), `已修改 ${shortId(card.id, 'card')} 的限额`);
    if (result === null || result === '') setLimiting(null);
    return result || null;
  };

  const rekey = async (card: AdminCardItem, preface?: string) => {
    if (unavailable || pending) return;
    const answer = await ask({
      title: `更换卡密 ${shortId(card.id, 'card')}？`,
      facts: [...facts(card), card.boundDevices.length ? `已绑定 ${card.boundDevices.length} 台设备（不变）` : '未绑定设备'],
      body: preface ? <p className="confirm-hint">{preface}</p> : undefined,
      consequence: '旧卡密马上失效，这张卡的所有登录马上退出，客户要用新卡密重新登录。余额、有效期、设备和记录都不变。新卡密只在下一步显示。',
      confirmLabel: '更换卡密', danger: true, typed: typedCardId(card.id),
      reason: reasonField('例：客户说卡密泄露了', ['卡密泄露', '电脑丢失或被盗', '客户要求']),
    });
    if (!answer.confirmed || !mounted.current) return;
    let code = '', changed: AdminCardItem | undefined;
    const failure = await send('更换卡密', [card.id], async () => {
      const result = await adminApi.rekeyCard(card.id, answer.reason);
      code = typeof result.rawCode === 'string' ? result.rawCode : ''; changed = result.card;
      return result;
    }, `已更换 ${shortId(card.id, 'card')} 的卡密`);
    if (failure) {reportError(`没有更换卡密：${failure}`); return;}
    if (failure !== null || !mounted.current) return;
    if (!code) {reportError('卡密已经更换，但服务器没有返回新卡密：请用“显示卡密”查看，或再换一次。'); return;}
    setRekeyed({card: changed ?? card, code});
  };

  const refreshForReview = async () => {
    if (checking || writing.current) return;
    setChecking(true); setReview('refresh');
    try {
      const result = await adminApi.getCards();
      if (!result.success) throw new Error('服务端未确认卡密列表');
      if (!mounted.current) return;
      updateCards(result.cards); setReview('review'); reportError('');
    } catch {reportError('卡密核对刷新失败，仍不能修改卡密，请重试。');}
    finally {if (mounted.current) setChecking(false);}
  };
  const release = async () => {
    if (!(await confirmAction({title: '确认已核对这些卡？', consequence: '上次的修改如果已经生效，请不要重复操作（延长有效期会再加一次）。解除后不会自动提交。', confirmLabel: '继续'}))) return;
    try {sessionStorage.removeItem(CARD_CHANGE_KEY); setPending(null); reportError('');}
    catch {reportError('无法清除待核对记录，仍不能修改卡密。请检查浏览器存储。');}
  };

  const view = <>
    {pending && <section className="recovery-panel" aria-label="卡密修改结果核对">
      <div>
        <h3>上次的卡密修改没收到结果</h3>
        <p>{pending.label}{pending.cardIds.length ? <> · {pending.cardIds.length === 1 ? <IdCell value={pending.cardIds[0]} kind="card"/> : `${formatCount(pending.cardIds.length)} 张卡`}</> : null}：可能已经生效。刷新列表核对后再继续。</p>
      </div>
      <div className="button-row">
        {pending.cardIds.length === 1 && <button type="button" className="btn btn-small" onClick={() => onShowCards(pending.cardIds)}>查看这张卡</button>}
        <button type="button" className="btn btn-small" disabled={checking} onClick={() => void refreshForReview()}>{checking ? '正在刷新…' : '刷新列表'}</button>
        <button type="button" className="btn btn-small" disabled={checking || review !== 'review'} title={review !== 'review' ? '先刷新列表' : undefined} onClick={() => void release()}>已核对，继续</button>
      </div>
    </section>}
    {extending && <ExtendDialog targets={extending.cards} everyResult={extending.everyResult} onClose={() => setExtending(null)} onSubmit={submitExtension}/>}
    {grouping && <GroupDialog card={grouping} groups={groups} groupName={groupName} onClose={() => setGrouping(null)}
      onSubmit={(groupId, reason) => submitGroup(grouping, groupId, reason)}/>}
    {rekeyed && <NewCodeDialog card={rekeyed.card} code={rekeyed.code} onClose={() => setRekeyed(null)}/>}
    {limiting && <QuotaDialog card={limiting} onClose={() => setLimiting(null)} onSubmit={(change, reason) => submitQuotas(limiting, change, reason)}/>}
  </>;

  return {blockedTitle, unban: card => void unban(card), unbind: (card, device) => void unbind(card, device), resetRebinds: card => void resetRebinds(card),
    extend, saveNote, changeGroup, changeQuotas, rekey: (card, preface) => void rekey(card, preface), revision, view};
}
