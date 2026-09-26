// One card at a glance: its facts, what happened to it (who and why), and its latest
// requests, with the everyday actions at the bottom. Actions reuse the list's handlers, so
// every confirmation and guard stays exactly as it is in the table.
import {useEffect, useRef, useState} from 'react';
import {adminApi, AdminApiError, type AdminCardItem, type AdminTrace, type CardEvent} from '../api';
import {IconChevronDown, IconChevronUp, IconClose, IconCopy} from '../components/icons';
import {Drawer} from '../components/modal';
import {copyText, IdCell, StatusBadge, Tag} from '../components/ui';
import {formatBatchNote, formatCharge, formatCount, formatCredits, formatDateTime, formatFullDateTime, formatRemaining, shortId} from '../format';
import {cardStatusView, traceStatusView} from '../status';

const ACTION_LABEL: Record<string, string> = {
  issued: '发卡', activated: '激活', topup: '充值', adjust: '调账', freeze: '冻结', unfreeze: '解冻',
  ban: '封禁', void: '永久作废', archive: '归档', unarchive: '取消归档',
};

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

export default function CardDrawer({card, groupName, hasPrev, hasNext, onMove, onClose, onReveal, onAdjust, onStatus, onOpenTrace, revealDisabled, blocked, blockedTitle}: {
  card: AdminCardItem;
  groupName: (id: string) => string;
  hasPrev: boolean;
  hasNext: boolean;
  onMove: (step: number) => void;
  onClose: () => void;
  onReveal: (card: AdminCardItem) => void;
  onAdjust: (card: AdminCardItem) => void;
  onStatus: (card: AdminCardItem, action: 'freeze' | 'unfreeze' | 'ban') => void;
  /** Opens 调用追踪 for this card, with one request's details open when given. */
  onOpenTrace: (cardId: string, traceId?: string) => void;
  revealDisabled: boolean;
  blocked: boolean;
  blockedTitle?: string;
}) {
  const view = cardStatusView(card.status);
  // Reloaded when the card changes (after an action from here or the list).
  const [history, retryHistory] = useLoad(async () => {
    const result = await adminApi.getCardHistory(card.id);
    if (result.success !== true || !Array.isArray(result.events)) throw new Error('服务器未确认读取成功');
    return result.events;
  }, [card.id, card.status, card.pointsAvailable, card.archivedAt]);
  const [recent, retryRecent] = useLoad(async () => {
    const result = await adminApi.getTraces(20, card.id);
    if (result.success !== true) throw new Error('服务器未确认读取成功');
    return result.traces as AdminTrace[];
  }, [card.id]);
  const remaining = card.validUntil ? formatRemaining(card.validUntil) : null;
  const body = useRef<HTMLDivElement>(null);
  useEffect(() => {body.current?.scrollTo?.({top: 0});}, [card.id]);

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
        <dt>备注</dt><dd title={card.note || undefined}>{card.note ? formatBatchNote(card.note) : <span className="muted">—</span>}</dd>
        <dt>分组</dt><dd>{groupName(card.groupId)}</dd>
        <dt>余额</dt><dd><b>{formatCredits(card.pointsAvailable)}</b> / {formatCredits(card.pointsTotal)} 积分</dd>
        <dt>到期</dt><dd>{card.validUntil
          ? <span title={formatFullDateTime(card.validUntil)}>{formatDateTime(card.validUntil)}{remaining && <span className={`remaining is-${remaining.tone}`}>（{remaining.text}）</span>}</span>
          : <span className="muted">激活后起算</span>}</dd>
        <dt>激活时间</dt><dd>{card.activatedAt ? <span title={formatFullDateTime(card.activatedAt)}>{formatDateTime(card.activatedAt)}</span> : <span className="muted">未激活</span>}</dd>
        <dt>设备</dt><dd>{card.boundDevices?.length
          ? <span className="device-list">{card.boundDevices.map(device => <IdCell key={device} value={device} kind="device"/>)}<span className="muted">（最多 {card.maxDevices} 台）</span></span>
          : <span className="muted">未绑定</span>}</dd>
      </dl>

      <section className="drawer-section" aria-label="操作记录">
        <h4>操作记录</h4>
        {history.status === 'loading' && <div className="skeleton" role="status" aria-label="正在加载"><span className="skeleton-bar"/><span className="skeleton-bar"/></div>}
        {history.status === 'missing' && <p className="empty-note">{history.message}</p>}
        {history.status === 'error' && <div className="inline-state"><span>读取失败：{history.message}</span><button type="button" className="btn btn-small" onClick={retryHistory}>重试</button></div>}
        {history.status === 'loaded' && (history.value.length
          ? <div className="table-scroll"><table className="table table-compact history-table">
            <thead><tr><th>时间</th><th>动作</th><th className="num">积分</th><th>操作人</th><th>原因</th></tr></thead>
            <tbody>{history.value.map((event, index) => <tr key={index}>
              <td title={formatFullDateTime(event.ts)}>{formatDateTime(event.ts)}</td>
              <td>{ACTION_LABEL[event.action] ?? event.action}</td>
              <td className={`num${event.action !== 'issued' && event.points < 0 ? ' is-negative' : ''}`}>{points(event)}</td>
              <td>{event.operator === 'system' ? '系统自动' : event.operator ?? '—'}</td>
              <td className="col-reason" title={event.reason ?? undefined}><span className="clip clip-reason">{event.reason ?? '—'}</span></td>
            </tr>)}</tbody>
          </table></div>
          : <p className="empty-note">没有记录</p>)}
      </section>

      <section className="drawer-section" aria-label="最近调用">
        <div className="section-head">
          <h4>最近调用</h4>
          <button type="button" className="btn-text" onClick={() => onOpenTrace(card.id)}>在调用追踪中查看</button>
        </div>
        {recent.status === 'loading' && <div className="skeleton" role="status" aria-label="正在加载"><span className="skeleton-bar"/></div>}
        {(recent.status === 'missing' || recent.status === 'error') && <div className="inline-state"><span>读取失败：{recent.message}</span><button type="button" className="btn btn-small" onClick={retryRecent}>重试</button></div>}
        {recent.status === 'loaded' && (recent.value.length
          ? <div className="table-scroll"><table className="table table-compact recent-table">
            <thead><tr><th>时间</th><th>模型</th><th className="col-status">结果</th><th className="num">扣费</th></tr></thead>
            <tbody>{recent.value.map(trace => <tr key={trace.id} className="is-clickable" title="查看这次请求" onClick={() => onOpenTrace(card.id, trace.id)}>
              <td title={formatFullDateTime(trace.ts)}>{formatDateTime(trace.ts)}</td>
              <td><span className="clip clip-model">{trace.exposed_model ?? '—'}</span></td>
              <td className="col-status"><StatusBadge view={traceStatusView(trace.status)}/></td>
              <td className="num">{formatCharge(trace.credits_charged)}</td>
            </tr>)}</tbody>
          </table></div>
          : <p className="empty-note">还没有调用</p>)}
        {recent.status === 'loaded' && recent.value.length >= 20 && <p className="muted">只显示最近 {formatCount(20)} 次</p>}
      </section>
    </div>
    <footer className="drawer-foot">
      <button type="button" className="btn" disabled={revealDisabled || !card.codeRecoverable} title={card.codeRecoverable ? undefined : '此卡未保存明文'} onClick={() => onReveal(card)}>查看卡密</button>
      <button type="button" className="btn" disabled={card.status === 'voided' || blocked} title={card.status === 'voided' ? '已作废，不能调账' : blockedTitle} onClick={() => onAdjust(card)}>调账</button>
      {card.status === 'active' && <button type="button" className="btn" disabled={blocked} title={blockedTitle} onClick={() => onStatus(card, 'freeze')}>冻结</button>}
      {card.status === 'frozen' && <button type="button" className="btn" disabled={blocked} title={blockedTitle} onClick={() => onStatus(card, 'unfreeze')}>解冻</button>}
      {!['banned', 'voided'].includes(card.status) && <button type="button" className="btn btn-danger" disabled={blocked} title={blockedTitle} onClick={() => onStatus(card, 'ban')}>封禁</button>}
      <span className="muted drawer-foot-id">{shortId(card.id, 'card')}</span>
    </footer>
  </Drawer>;
}
