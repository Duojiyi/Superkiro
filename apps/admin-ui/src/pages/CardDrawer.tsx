// One card at a glance: its facts, what happened to it (who and why), and its latest
// requests, with the everyday actions at the bottom and the support ones beside the facts they
// change (备注, 分组, 到期, 设备). Actions reuse the list's handlers, so every confirmation and
// guard stays exactly as it is in the table.
import {useEffect, useRef, useState} from 'react';
import {adminApi, AdminApiError, type AdminCardItem, type AdminTrace, type CardEvent} from '../api';
import {daysText, historyDetail, historyLabel, noteProblem, rebindsToReset, rebindText} from '../cardSupport';
import {IconChevronDown, IconChevronUp, IconClose, IconCopy} from '../components/icons';
import {Drawer} from '../components/modal';
import {copyText, IdCell, StatusBadge, Tag} from '../components/ui';
import {formatBatchNote, formatCharge, formatCount, formatCredits, formatDateTime, formatExpired, formatFullDateTime, formatRemaining, shortId} from '../format';
import {cardStatusView, traceStatusView, traceStuck} from '../status';
import type {CardSupport} from './CardSupport';

/** 编辑备注 in place: Enter saves, Escape puts the note back; a refusal stays to be corrected. */
function NoteEditor({card, disabled, disabledTitle, onSave}: {card: AdminCardItem; disabled: boolean; disabledTitle?: string; onSave: (note: string) => Promise<string | null>}) {
  const [draft, setDraft] = useState<string | null>(null);
  const [error, setError] = useState('');
  const [saving, setSaving] = useState(false);
  useEffect(() => {setDraft(null); setError('');}, [card.id]);
  const problem = draft === null ? '' : noteProblem(draft);
  const save = async () => {
    if (draft === null || saving) return;
    if (problem) {setError(problem); return;}
    if (draft.trim() === (card.note ?? '').trim()) {setDraft(null); return;}
    setSaving(true); setError('');
    const failure = await onSave(draft);
    setSaving(false);
    if (failure) setError(failure); else setDraft(null);
  };
  if (draft === null) return <>
    {card.note ? <span title={card.note}>{formatBatchNote(card.note)}</span> : <span className="muted">—</span>}
    <button type="button" className="btn-text" disabled={disabled} title={disabledTitle} onClick={() => setDraft(card.note ?? '')}>编辑</button>
  </>;
  return <span className="note-editor">
    <input aria-label="备注" value={draft} autoFocus disabled={saving} placeholder="留空即清除备注" onChange={event => {setDraft(event.target.value); setError('');}}
      onKeyDown={event => {
        if (event.key === 'Enter' && !event.nativeEvent.isComposing) {event.preventDefault(); void save();}
        // Escape leaves the note as it was, not the drawer.
        if (event.key === 'Escape') {event.preventDefault(); setDraft(null); setError('');}
      }}/>
    <button type="button" className="btn btn-small btn-primary" disabled={saving || !!problem} onClick={() => void save()}>{saving ? '保存中…' : '保存'}</button>
    <button type="button" className="btn btn-small" disabled={saving} onClick={() => {setDraft(null); setError('');}}>取消</button>
    {(error || problem) && <span role="alert" className="field-error">{error || problem}</span>}
  </span>;
}

type Load<T> = {status: 'loading'} | {status: 'loaded'; value: T} | {status: 'missing' | 'error'; message: string};

function useLoad<T>(load: () => Promise<T>, deps: unknown[]): [Load<T>, () => void] {
  const [state, setState] = useState<Load<T>>({status: 'loading'});
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let current = true;
    setState({status: 'loading'});
    load().then(value => {if (current) setState({status: 'loaded', value});}).catch(error => {
      if (!current) return;
      setState(error instanceof AdminApiError && error.status === 404 ? {status: 'missing', message: error.message} : {status: 'error', message: error instanceof Error ? error.message : String(error)});
    });
    return () => {current = false;};
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, attempt]);
  return [state, () => setAttempt(value => value + 1)];
}

function points(event: CardEvent): string {
  if (!event.points) return '';
  if (event.action === 'issued') return formatCredits(event.points);
  return `${event.points > 0 ? '+' : ''}${formatCredits(event.points)}`;
}

export default function CardDrawer({card, state, groupName, hasPrev, hasNext, onMove, onClose, onReveal, onAdjust, onCompensate, onStatus, onOpenTrace, revealDisabled, blocked, blockedTitle, support}: {
  card: AdminCardItem;
  /** Its status as it works now (a card past its date is expired). */
  state: AdminCardItem['status'];
  groupName: (id: string) => string;
  hasPrev: boolean;
  hasNext: boolean;
  onMove: (step: number) => void;
  onClose: () => void;
  onReveal: (card: AdminCardItem) => void;
  onAdjust: (card: AdminCardItem) => void;
  /** 调账 with what these requests charged (补偿选中的请求). */
  onCompensate: (card: AdminCardItem, requests: AdminTrace[]) => void;
  onStatus: (card: AdminCardItem, action: 'freeze' | 'unfreeze' | 'ban') => void;
  /** Opens 调用追踪 for this card, with one request's details open when given (by its ID, or its invocation ID). */
  onOpenTrace: (cardId: string, traceId?: string, invocationId?: string) => void;
  revealDisabled: boolean;
  blocked: boolean;
  blockedTitle?: string;
  /** 解封, 解绑设备, 重置换绑次数, 延长有效期, 备注 and 换分组. */
  support: CardSupport;
}) {
  const view = cardStatusView(state);
  // Reloaded when the card changes (after an action from here or the list).
  const [history, retryHistory] = useLoad(async () => {
    const result = await adminApi.getCardHistory(card.id);
    if (result.success !== true || !Array.isArray(result.events)) throw new Error('服务器未确认读取成功');
    return result.events;
  }, [card.id, JSON.stringify([card.status, card.pointsAvailable, card.archivedAt, card.note, card.groupId, card.validUntil, card.activationDurationSecs, card.boundDevices, card.rebindsUsed, card.rebindCooldownUntil])]);
  const [recent, retryRecent] = useLoad(async () => {
    const result = await adminApi.getTraces(20, card.id);
    if (result.success !== true) throw new Error('服务器未确认读取成功');
    return result.traces as AdminTrace[];
  }, [card.id]);
  const nowSecs = Date.now() / 1000;
  const remaining = card.validUntil ? formatRemaining(card.validUntil) : null;
  const lapsed = state === 'expired' && card.validUntil != null && card.validUntil * 1000 <= Date.now();
  const body = useRef<HTMLDivElement>(null);
  useEffect(() => {body.current?.scrollTo?.({top: 0});}, [card.id]);
  // Support actions wait for the list and for a result still to be checked; a voided card has none.
  const supportBlocked = blocked || !!support.blockedTitle;
  const supportTitle = support.blockedTitle ?? blockedTitle;
  const voided = card.status === 'voided';
  // A card that never expires (no date once activated, a zero validity before) has nothing to extend.
  const extendable = !voided && card.archivedAt == null && (card.validUntil != null || (card.activatedAt == null && card.activationDurationSecs !== 0));
  const allowance = rebindText(card, nowSecs);
  // Requests ticked in 最近调用 to give back what they charged; only charged ones can be.
  const [picked, setPicked] = useState<string[]>([]);
  useEffect(() => {setPicked([]);}, [card.id, recent]);
  const pickedRequests = recent.status === 'loaded' ? recent.value.filter(trace => picked.includes(trace.id)) : [];
  const pickedMicro = pickedRequests.reduce((sum, trace) => sum + Number(trace.credits_charged ?? 0), 0);
  // An adjustment that made up for a request leads back to it: the recent one here, else on 调用追踪.
  const openRequest = (invocationId: string) => {
    const trace = recent.status === 'loaded' ? recent.value.find(item => item.invocation_id === invocationId) : undefined;
    onOpenTrace(card.id, trace?.id, trace ? undefined : invocationId);
  };

  return <Drawer id="card-detail" label="卡密详情" onClose={onClose}>
    <header className="drawer-head">
      <div className="drawer-title">
        <span className="mono drawer-model" title={card.id}>{card.id}</span>
        <button type="button" className="btn-icon" aria-label="复制卡密 ID" title="复制卡密 ID" onClick={() => void copyText(card.id, '已复制卡密 ID')}><IconCopy/></button>
        <StatusBadge view={view}/>{card.archivedAt != null && <Tag>已归档</Tag>}
      </div>
      <div className="drawer-tools">
        <button type="button" className="btn-icon" aria-label="上一张" title="上一张（↑）" disabled={!hasPrev} onClick={() => onMove(-1)}><IconChevronUp/></button>
        <button type="button" className="btn-icon" aria-label="下一张" title="下一张（↓）" disabled={!hasNext} onClick={() => onMove(1)}><IconChevronDown/></button>
        <button type="button" className="btn-icon" aria-label="关闭详情" title="关闭（Esc）" onClick={onClose}><IconClose/></button>
      </div>
    </header>
    <div className="drawer-body" ref={body}>
      <dl className="detail-list">
        <dt>备注</dt><dd><NoteEditor card={card} disabled={supportBlocked || voided} disabledTitle={voided ? '已作废' : supportTitle} onSave={note => support.saveNote(card, note)}/></dd>
        <dt>分组</dt><dd>{groupName(card.groupId)}
          {!voided && <button type="button" className="btn-text" disabled={supportBlocked} title={supportTitle ?? '换到别的分组（客户需要重新登录）'} onClick={() => support.changeGroup(card)}>换分组</button>}</dd>
        <dt>余额</dt><dd><b>{formatCredits(card.pointsAvailable)}</b> / {formatCredits(card.pointsTotal)} 积分</dd>
        <dt>到期</dt><dd>{card.validUntil
          ? <span title={formatFullDateTime(card.validUntil)}>{formatDateTime(card.validUntil)}{lapsed
            // What the date means for the customer: how long ago, and the balance they can no longer use.
            ? <span className="remaining is-danger">（{formatExpired(card.validUntil)}{card.pointsAvailable > 0 ? ` · ${formatCredits(card.pointsAvailable)} 积分已不可用` : ''}）</span>
            : remaining && <span className={`remaining is-${remaining.tone}`}>（{remaining.text}）</span>}</span>
          : <span className="muted">{typeof card.activationDurationSecs === 'number' && card.activationDurationSecs > 0 ? `激活后起算 · 有效 ${daysText(card.activationDurationSecs)}` : '激活后起算'}</span>}
          {extendable && <button type="button" className="btn-text" disabled={supportBlocked} title={supportTitle ?? '延长这张卡的有效期'} onClick={() => support.extend([card])}>延长</button>}</dd>
        <dt>激活时间</dt><dd>{card.activatedAt ? <span title={formatFullDateTime(card.activatedAt)}>{formatDateTime(card.activatedAt)}</span> : <span className="muted">未激活</span>}</dd>
        <dt>设备</dt><dd className="device-block">
          {card.boundDevices?.length
            ? <span className="device-list">{card.boundDevices.map(device => <span key={device} className="device-item"><IdCell value={device} kind="device"/>
              <button type="button" className="btn-text" disabled={supportBlocked} title={supportTitle ?? '解绑这台设备（不占用客户的换绑次数）'} onClick={() => support.unbind(card, device)}>解绑</button></span>)}
              <span className="muted">（最多 {card.maxDevices} 台）</span></span>
            : <span className="muted">未绑定</span>}
          {allowance && <span className="rebind-line">
            <span className={Number(card.rebindCooldownUntil) > nowSecs || (card.rebindsUsed ?? 0) >= (card.maxRebinds ?? Infinity) ? 'is-warning' : 'muted'}>{allowance}</span>
            {rebindsToReset(card, nowSecs) && <button type="button" className="btn-text" disabled={supportBlocked} title={supportTitle ?? '让客户重新有全部的自助换绑次数'} onClick={() => support.resetRebinds(card)}>重置换绑次数</button>}
          </span>}
        </dd>
      </dl>

      <section className="drawer-section" aria-label="操作记录">
        <h4>操作记录</h4>
        {history.status === 'loading' && <div className="skeleton" role="status" aria-label="正在加载"><span className="skeleton-bar"/><span className="skeleton-bar"/></div>}
        {history.status === 'missing' && <p className="empty-note">{history.message}</p>}
        {history.status === 'error' && <div className="inline-state"><span>读取失败：{history.message}</span><button type="button" className="btn btn-small" onClick={retryHistory}>重试</button></div>}
        {history.status === 'loaded' && (history.value.length
          ? <div className="table-scroll"><table className="table table-compact history-table">
            <thead><tr><th>时间</th><th>动作</th><th className="num">积分</th><th>操作人</th><th>原因</th></tr></thead>
            <tbody>{history.value.map((event, index) => {
              // What the change recorded besides who and why: the device, the new expiry, the groups.
              const detail = historyDetail(event, groupName);
              return <tr key={index}>
                <td title={formatFullDateTime(event.ts)}>{formatDateTime(event.ts)}</td>
                <td>{historyLabel(event.action)}{detail && <span className="history-detail" title={typeof event.detail?.deviceId === 'string' ? event.detail.deviceId : detail}>{detail}</span>}
                  {event.invocationId && <button type="button" className="btn-text history-link" title={`打开这笔调账补偿的请求（${event.invocationId}）`} onClick={() => openRequest(String(event.invocationId))}>查看请求</button>}</td>
                <td className={`num${event.action !== 'issued' && event.points < 0 ? ' is-negative' : ''}`}>{points(event)}</td>
                <td>{event.operator === 'system' ? '系统自动' : event.operator ?? '—'}</td>
                <td className="col-reason" title={event.reason ?? undefined}><span className="clip clip-reason">{event.reason ?? '—'}</span></td>
              </tr>;
            })}</tbody>
          </table></div>
          : <p className="empty-note">没有记录</p>)}
      </section>

      <section className="drawer-section" aria-label="最近调用">
        <div className="section-head">
          <h4>最近调用</h4>
          <span className="section-tools">
            {pickedRequests.length > 0 && <button type="button" className="btn btn-small" disabled={blocked || voided} title={voided ? '已作废，不能调账' : blockedTitle}
              onClick={() => onCompensate(card, pickedRequests)}>补偿选中的 {pickedRequests.length} 次（{formatCharge(pickedMicro)} 积分）</button>}
            <button type="button" className="btn-text" onClick={() => onOpenTrace(card.id)}>在调用追踪中查看</button>
          </span>
        </div>
        {recent.status === 'loading' && <div className="skeleton" role="status" aria-label="正在加载"><span className="skeleton-bar"/></div>}
        {(recent.status === 'missing' || recent.status === 'error') && <div className="inline-state"><span>读取失败：{recent.message}</span><button type="button" className="btn btn-small" onClick={retryRecent}>重试</button></div>}
        {recent.status === 'loaded' && (recent.value.length
          ? <div className="table-scroll"><table className="table table-compact recent-table">
            <thead><tr><th className="col-check"><span className="sr-only">补偿</span></th><th>时间</th><th>模型</th><th className="col-status">结果</th><th className="num">扣费</th></tr></thead>
            <tbody>{recent.value.map(trace => {
              const charged = Number(trace.credits_charged ?? 0) > 0;
              // Ticking a request to compensate is not a request for its details.
              return <tr key={trace.id} className="is-clickable" title="查看这次请求" onClick={event => {if (!(event.target instanceof Element && event.target.closest('input'))) onOpenTrace(card.id, trace.id);}}>
              <td className="col-check"><input type="checkbox" aria-label={`补偿 ${formatDateTime(trace.ts)} 的请求`} disabled={!charged} title={charged ? '选中后可一起补偿扣费' : '这次请求没有扣费'}
                checked={picked.includes(trace.id)} onChange={event => {const on = event.currentTarget.checked; setPicked(ids => on ? [...ids, trace.id] : ids.filter(id => id !== trace.id));}}/></td>
              <td title={formatFullDateTime(trace.ts)}>{formatDateTime(trace.ts)}</td>
              <td><span className="clip clip-model">{trace.exposed_model ?? '—'}</span></td>
              <td className="col-status"><StatusBadge view={traceStatusView(trace.status, traceStuck(trace, Date.now() / 1000))}/></td>
              <td className="num">{formatCharge(trace.credits_charged)}</td>
            </tr>;
            })}</tbody>
          </table></div>
          : <p className="empty-note">还没有调用</p>)}
        {recent.status === 'loaded' && recent.value.length >= 20 && <p className="muted">只显示最近 {formatCount(20)} 次</p>}
      </section>
    </div>
    <footer className="drawer-foot">
      <button type="button" className="btn" disabled={revealDisabled || !card.codeRecoverable} title={card.codeRecoverable ? '显示这张卡的卡密原文（会记录）' : '此卡未保存明文'} onClick={() => onReveal(card)}>显示卡密</button>
      <button type="button" className="btn" disabled={card.status === 'voided' || blocked} title={card.status === 'voided' ? '已作废，不能调账' : blockedTitle} onClick={() => onAdjust(card)}>调账</button>
      {state === 'active' && <button type="button" className="btn" disabled={blocked} title={blockedTitle} onClick={() => onStatus(card, 'freeze')}>冻结</button>}
      {state === 'frozen' && <button type="button" className="btn" disabled={blocked} title={blockedTitle} onClick={() => onStatus(card, 'unfreeze')}>解冻</button>}
      {card.status === 'banned' && <button type="button" className="btn" disabled={supportBlocked || card.archivedAt != null}
        title={card.archivedAt != null ? '已归档的卡要先取消归档，再解封' : supportTitle ?? '恢复使用（需填写原因）'} onClick={() => support.unban(card)}>解封</button>}
      {!['banned', 'voided'].includes(state) && <button type="button" className="btn btn-danger" disabled={blocked} title={blockedTitle} onClick={() => onStatus(card, 'ban')}>封禁</button>}
      <span className="muted drawer-foot-id">{shortId(card.id, 'card')}</span>
    </footer>
  </Drawer>;
}
