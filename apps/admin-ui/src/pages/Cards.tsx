// 卡密资产: one line per card, status tabs with counts, a selection bar that appears only
// when cards are selected, and the dialogs to issue, view and adjust cards.
import {useEffect, useRef, useState} from 'react';
import {clearAdjustment, isUnsubmittedAdjustmentRejection, isZeroMicroAdjustment, loadAdjustment, saveAdjustment, type Adjustment} from '../adjustment';
import {adminApi, AdminApiError, type AdminCardItem, type GeneratedCard} from '../api';
import {ask, confirmAction} from '../components/confirm';
import {IconChevronDown, IconClose, IconSearch, IconSortDown, IconSortUp} from '../components/icons';
import {Menu, type MenuItem} from '../components/menu';
import {Modal} from '../components/modal';
import {toast} from '../components/toast';
import {FilterTabs, IdCell, Pager, StatusBadge, TableState, Tag, TopbarActions, type TabOption} from '../components/ui';
import {formatBatchNote, formatCount, formatCredits, formatFullDateTime, formatMoney, formatRemaining, shortId} from '../format';
import {adjustmentPointsToMicro} from '../pricing';
import {cardStatusView} from '../status';
import type {CardQuickFilter, CardTab, ErrorAction, Refresh, ReportError, Row, WriteGuards} from '../types';

const TIERS = [
  {id: 'tier-1000', name: 'PRO', points: 1000, price_cny: 30},
  {id: 'tier-2000', name: 'PRO+', points: 2000, price_cny: 55},
  {id: 'tier-5000', name: 'PRO Max', points: 5000, price_cny: 130},
  {id: 'tier-10000', name: 'Power', points: 10000, price_cny: 250},
];
const PAGE_SIZE = 50;
const DAY = 86400;
const ISSUANCE_KEY = 'admin-pending-issuance:v1';
const REASONS = ['测试卡清理', '退款', '滥用', '客户要求'];

type BulkAction = 'freeze' | 'unfreeze' | 'ban' | 'void' | 'archive' | 'unarchive' | 'export';
const VERB: Record<BulkAction, string> = {freeze: '冻结', unfreeze: '解冻', ban: '封禁', void: '永久作废', archive: '归档', unarchive: '取消归档', export: '导出明文'};
const BULK_CONSEQUENCE: Record<BulkAction, string> = {
  freeze: '客户将暂时无法使用，可随时解冻。',
  unfreeze: '恢复使用，仍受有效期和余额限制。',
  ban: '封禁后不能恢复，也不会自动退款。',
  void: '不能恢复，余额作废且不退款（财务与审计记录保留）；有进行中请求的卡会被拒绝，请先冻结再作废。',
  archive: '只从列表中隐藏，卡的状态和余额不变（只能归档已封禁、已到期或已作废的卡）。',
  unarchive: '重新显示在当前列表中，不会恢复使用权限。',
  export: '文件含明文卡密，请妥善保管。',
};

// Refused by the server's validation or policy: nothing was written, so no review is needed.
const refused = (error: unknown) => error instanceof AdminApiError && [400, 403, 404, 409, 413, 422].includes(error.status);
const errorText = (error: unknown) => (error instanceof Error ? error.message : String(error));

function matchesTab(card: AdminCardItem, tab: CardTab): boolean {
  const archived = card.archivedAt != null;
  switch (tab) {
    case 'CURRENT': return !archived && card.status !== 'voided';
    case 'ARCHIVED': return archived;
    case 'VOIDED': return card.status === 'voided';
    case 'ALL': return true;
    default: return !archived && card.status.toUpperCase() === tab;
  }
}

function matchesQuick(card: AdminCardItem, quick: CardQuickFilter | null, nowSecs: number): boolean {
  if (!quick) return true;
  if (card.archivedAt != null) return false;
  if (quick === 'expiring') return ['active', 'frozen'].includes(card.status) && card.validUntil != null && card.validUntil > nowSecs && card.validUntil <= nowSecs + 7 * DAY;
  return card.status === 'active' && card.pointsTotal > 0 && card.pointsAvailable / card.pointsTotal < 0.1;
}

function matchesSearch(card: AdminCardItem, query: string): boolean {
  const text = query.trim().toLowerCase();
  if (!text) return true;
  return card.id.toLowerCase().includes(text) || !!card.note?.toLowerCase().includes(text) || card.boundDevices.some(device => device.toLowerCase().includes(text));
}

function Balance({card}: {card: AdminCardItem}) {
  const ratio = card.pointsTotal > 0 ? Math.max(0, Math.min(1, card.pointsAvailable / card.pointsTotal)) : 0;
  const low = card.status === 'active' && card.pointsTotal > 0 && ratio < 0.1;
  return <span className="balance" title={`可用 ${formatCredits(card.pointsAvailable)} / 总额 ${formatCredits(card.pointsTotal)} 积分`}>
    <span className="balance-text"><b className={low ? 'is-warning' : undefined}>{formatCredits(card.pointsAvailable)}</b> / {formatCredits(card.pointsTotal)}</span>
    <span className={`meter${low ? ' is-low' : ''}`}><span style={{width: `${ratio * 100}%`}}/></span>
  </span>;
}

function Expiry({card, now}: {card: AdminCardItem; now: number}) {
  if (card.validUntil == null) return <span className="muted">{card.status === 'unactivated' ? '激活后起算' : '—'}</span>;
  const date = new Date(card.validUntil * 1000);
  const day = `${date.getFullYear() === new Date(now).getFullYear() ? '' : `${date.getFullYear()}-`}${String(date.getMonth() + 1).padStart(2, '0')}-${String(date.getDate()).padStart(2, '0')}`;
  const left = formatRemaining(card.validUntil, now);
  const live = ['active', 'frozen', 'unactivated'].includes(card.status);
  return <span title={formatFullDateTime(card.validUntil)}>{day}{live && <span className={`remaining is-${left.tone}`}>（{left.text}）</span>}</span>;
}

function Devices({card}: {card: AdminCardItem}) {
  if (!card.boundDevices.length) return <span className="muted">—</span>;
  return <span className="devices" title={card.boundDevices.join('\n')}>
    <IdCell value={card.boundDevices[0]} kind="device"/>
    {card.boundDevices.length > 1 && <span className="muted"> +{card.boundDevices.length - 1}</span>}
  </span>;
}

interface Generated {cards: GeneratedCard[]; tier: string; points: number; groupName: string}

export default function CardsPage({cards, groups, configFailed, loading, failed, operator, refresh, guards, reportError, actionError, onBusyChange, onReauthenticate, selectionEpoch, intent, updateCards}: {
  cards: AdminCardItem[];
  groups: Row[];
  configFailed: boolean;
  loading: boolean;
  failed: boolean;
  operator: string | null;
  refresh: Refresh;
  guards: WriteGuards;
  reportError: ReportError;
  actionError: string;
  onBusyChange: (busy: boolean) => void;
  onReauthenticate: () => void;
  selectionEpoch: number;
  intent?: {status?: CardTab; quick?: CardQuickFilter; search?: string};
  updateCards: (cards: AdminCardItem[]) => void;
}) {
  const {writing, mounted} = guards;
  const alive = useRef(true);
  useEffect(() => {alive.current = true; return () => {alive.current = false;};}, []);
  const now = Date.now(), nowSecs = now / 1000;

  // Filters, sorting, paging and selection.
  const [statusTab, setStatusTab] = useState<CardTab>(intent?.status ?? 'CURRENT');
  const [quick, setQuick] = useState<CardQuickFilter | null>(intent?.quick ?? null);
  const [search, setSearch] = useState(intent?.search ?? '');
  const [groupFilter, setGroupFilter] = useState('ALL');
  const [sort, setSort] = useState<{key: 'balance' | 'expiry'; direction: 1 | -1} | null>(null);
  const [page, setPage] = useState(0);
  const [selectedIds, setSelectedIds] = useState<string[]>([]);
  const [bulkBusy, setBulkBusy] = useState(false);
  const [progress, setProgress] = useState<{done: number; total: number} | null>(null);
  const [report, setReport] = useState<{summary: string; failures: Array<{id: string; result: string}>} | null>(null);
  const [mutationBusy, setMutationBusy] = useState(false);
  const [revealing, setRevealing] = useState(false);
  const [revealed, setRevealed] = useState<{cardId: string; rawCode: string} | null>(null);
  const [revealError, setRevealError] = useState('');

  // Issuing.
  const [showBatch, setShowBatch] = useState(false);
  const [batchCount, setBatchCount] = useState('50');
  const [batchGroup, setBatchGroup] = useState('');
  const [batchTemplate, setBatchTemplate] = useState('tier-2000');
  const [batchNote, setBatchNote] = useState('');
  const [generating, setGenerating] = useState(false);
  const [generated, setGenerated] = useState<Generated | null>(null);
  const [generatedError, setGeneratedError] = useState('');
  const [issuanceRecovery, setIssuanceRecovery] = useState<'refresh' | 'review' | null>(() => {
    try {return sessionStorage.getItem(ISSUANCE_KEY) ? 'refresh' : null;} catch {return 'refresh';}
  });
  const [issuanceReference, setIssuanceReference] = useState(() => {
    try {
      const reference = JSON.parse(sessionStorage.getItem(ISSUANCE_KEY) || '{}')?.reference;
      return typeof reference === 'string' && reference.length <= 256 ? reference : '';
    } catch {return '';}
  });
  const [issuanceChecking, setIssuanceChecking] = useState(false);

  // Adjusting. The intent is kept in sessionStorage per operator until the server confirms it.
  const [adjustCard, setAdjustCard] = useState<AdminCardItem | null>(null);
  const [adjustDirection, setAdjustDirection] = useState<'add' | 'deduct'>('add');
  const [adjustAmount, setAdjustAmount] = useState('');
  const [adjustReason, setAdjustReason] = useState('');
  const [adjustStep, setAdjustStep] = useState<'form' | 'review'>('form');
  const adjustment = useRef<Adjustment | null>(null);
  const [pendingIntent, setPendingIntent] = useState<Adjustment | null>(null);
  const setIntent = (intent: Adjustment | null) => {adjustment.current = intent; setPendingIntent(intent);};
  const adjusting = useRef(false);

  // New card codes are shown once: while they are on screen, leaving or reloading asks first.
  useEffect(() => {onBusyChange(bulkBusy || generating || !!generated); }, [bulkBusy, generating, generated, onBusyChange]);
  useEffect(() => () => onBusyChange(false), [onBusyChange]);

  useEffect(() => {
    setIntent(null);
    if (!operator) return;
    try {setIntent(loadAdjustment(sessionStorage, operator));}
    catch {reportError('无法读取保存的调账意图，请检查浏览器存储并人工核对账本；暂不允许新调账。');}
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [operator]);

  // Any change of filter, page or a fresh refresh clears the selection, so nothing unseen is acted on.
  useEffect(() => {setSelectedIds([]); setPage(0);}, [search, groupFilter, statusTab, quick, sort]);
  useEffect(() => {setSelectedIds([]);}, [page]);
  useEffect(() => {setSelectedIds([]);}, [selectionEpoch]);

  const groupName = (id: unknown) => {
    const group = groups.find(item => item.id === id);
    return String(group?.name ?? id ?? '—');
  };
  const scoped = cards.filter(card => (groupFilter === 'ALL' || card.groupId === groupFilter) && matchesSearch(card, search) && matchesQuick(card, quick, nowSecs));
  const filtered = scoped.filter(card => matchesTab(card, statusTab));
  if (sort) {
    const value = (card: AdminCardItem) => sort.key === 'balance' ? card.pointsAvailable : card.validUntil ?? Number.MAX_SAFE_INTEGER;
    filtered.sort((a, b) => (value(a) - value(b)) * sort.direction);
  }
  const pageCount = Math.max(1, Math.ceil(filtered.length / PAGE_SIZE));
  const currentPage = Math.min(page, pageCount - 1);
  const pageCards = filtered.slice(currentPage * PAGE_SIZE, (currentPage + 1) * PAGE_SIZE);
  const pageCardIds = JSON.stringify(pageCards.map(card => card.id));
  useEffect(() => {
    const visible: string[] = JSON.parse(pageCardIds);
    setSelectedIds(ids => ids.filter(id => visible.includes(id)));
  }, [pageCardIds]);
  const filtersActive = !!search.trim() || groupFilter !== 'ALL' || statusTab !== 'CURRENT' || !!quick;
  const resetFilters = () => {setSelectedIds([]); setSearch(''); setGroupFilter('ALL'); setStatusTab('CURRENT'); setQuick(null); setPage(0);};

  const tabCount = (tab: CardTab) => scoped.filter(card => matchesTab(card, tab)).length;
  const tabs: TabOption<CardTab>[] = ([
    ['CURRENT', '当前'], ['UNACTIVATED', '未激活'], ['ACTIVE', '使用中'], ['FROZEN', '已冻结'], ['BANNED', '已封禁'],
    ['EXPIRED', '已到期'], ['ARCHIVED', '已归档'], ['VOIDED', '已作废'], ['ALL', '全部'],
  ] as Array<[CardTab, string]>).map(([value, label]) => ({value, label, count: tabCount(value), tone: value === 'FROZEN' ? 'warning' : undefined}));
  const quickCount = (value: CardQuickFilter) => cards.filter(card => (groupFilter === 'ALL' || card.groupId === groupFilter) && matchesSearch(card, search) && matchesQuick(card, value, nowSecs)).length;

  const blocked = bulkBusy || loading || failed || mutationBusy;
  const staleTitle = failed ? '卡密列表没有刷新成功，暂不能操作' : undefined;
  const reauthenticate: ErrorAction = {label: '重新登录', run: onReauthenticate};

  // ---- Single-card status changes ----
  const changeStatus = async (card: AdminCardItem, action: 'freeze' | 'unfreeze' | 'ban') => {
    if (writing.current || bulkBusy || loading || failed) return;
    const verb = VERB[action];
    const answer = await ask({
      title: `${verb}卡密 ${shortId(card.id, 'card')}？`,
      facts: [`余额 ${formatCredits(card.pointsAvailable)} / ${formatCredits(card.pointsTotal)} 积分 · ${cardStatusView(card.status).label}`, ...(card.note ? [`备注：${formatBatchNote(card.note)}`] : [])],
      consequence: {freeze: '客户将暂时无法使用，可随时解冻。', unfreeze: '恢复使用，仍受有效期和余额限制。', ban: '封禁后不能恢复，也不会自动退款。'}[action],
      confirmLabel: verb,
      danger: action === 'ban',
      reason: {label: '原因（可选）', placeholder: `不填写时记为“管理员手动操作：${verb}”`, suggestions: action === 'unfreeze' ? ['核实无误', '客户要求'] : REASONS, maxLength: 100},
    });
    if (!answer.confirmed || !alive.current || writing.current) return;
    try {
      writing.current = true; setMutationBusy(true); reportError('');
      const response = await adminApi.updateCardStatus(card.id, action, answer.reason || `管理员手动操作：${verb}`);
      if (!response.success) throw new Error('服务器未确认状态变更，请刷新核对。');
      toast.success(`已${verb} ${shortId(card.id, 'card')}`);
      void refresh();
    } catch (error) {
      reportError(`操作失败：${errorText(error)}`);
    } finally {writing.current = false; setMutationBusy(false);}
  };

  // ---- Bulk actions: only the ticked cards on the page in view ----
  const runBulk = async (action: BulkAction) => {
    if (writing.current || bulkBusy || loading || failed || mutationBusy || revealing) return;
    const targets = pageCards.filter(card => selectedIds.includes(card.id));
    if (!targets.length) return;
    if (action === 'void') {
      if (!operator) {reportError('需要重新登录以确认操作人；本次未发送作废请求。', reauthenticate); return;}
      try {
        const pending = loadAdjustment(sessionStorage, operator);
        if (pending && targets.some(card => card.id === pending.cardId)) {
          reportError(`卡 ${pending.cardId} 有未确认调账，请先核对原调账再作废；本次未发送作废请求。`); return;
        }
      } catch {reportError('调账恢复记录无法读取，请先核对账本；本次未发送作废请求。'); return;}
    }
    const verb = VERB[action];
    // Name what is about to change: a count alone let a mis-ticked page be voided.
    const named = targets.slice(0, 8).map(card => shortId(card.id, 'card')).join('、') + (targets.length > 8 ? ` 等 ${targets.length} 张` : '');
    const activated = targets.filter(card => card.status !== 'unactivated').length;
    const withBalance = targets.filter(card => card.creditTotal > card.creditUsed).length;
    const irreversible = action === 'ban' || action === 'void';
    const confirmed = await confirmAction({
      title: `${verb} ${targets.length} 张卡密？`,
      facts: [named, `其中 ${activated} 张已激活、${withBalance} 张有剩余额度`],
      consequence: BULK_CONSEQUENCE[action],
      confirmLabel: verb,
      danger: irreversible,
      typed: irreversible ? String(targets.length) : undefined,
    });
    if (!confirmed || !alive.current || writing.current || !mounted.current) return;
    writing.current = true; setBulkBusy(true); setReport(null); reportError('');
    setProgress({done: 0, total: targets.length});
    const failures: Array<{id: string; result: string}> = [];
    const keepSelected = new Set<string>();
    const codes: string[] = [];
    let succeeded = 0;
    try {
      for (let index = 0; index < targets.length; index++) {
        const card = targets[index];
        if (!alive.current) break;
        try {
          if (action === 'export') {
            if (!card.codeRecoverable) {failures.push({id: card.id, result: '失败：历史卡密不可恢复'}); continue;}
            const response = await adminApi.revealCard(card.id);
            if (!alive.current) break;
            if (!response.success || !response.rawCode) throw new Error('reveal failed');
            codes.push(response.rawCode);
          } else {
            const expired = card.validUntil != null && card.validUntil <= Date.now() / 1000;
            if ((action === 'archive' && (card.archivedAt != null || !(['banned', 'expired', 'voided'].includes(card.status) || expired))) || (action === 'unarchive' && card.archivedAt == null)) {
              failures.push({id: card.id, result: '未执行：不符合归档条件或已是目标状态'}); continue;
            }
            if ((action === 'freeze' && card.status !== 'active') || (action === 'unfreeze' && card.status !== 'frozen') || (action === 'ban' && ['banned', 'voided'].includes(card.status)) || (action === 'void' && card.status === 'voided')) {
              failures.push({id: card.id, result: '未执行：当前状态不适用该操作'}); continue;
            }
            const response = await adminApi.updateCardStatus(card.id, action, `管理员批量操作：${verb}`);
            if (!response.success) throw new Error('status failed');
            succeeded++;
          }
        } catch (error) {
          // Do not retain server error bodies in a secret-bearing operation.
          failures.push({id: card.id, result: action === 'export' ? '读取失败，请核对会话或卡密可恢复性' : '失败或结果未确认，请刷新核对后再操作'});
          if (error instanceof AdminApiError && (error.status === 401 || error.status === 403)) {
            for (const pending of targets.slice(index + 1)) failures.push({id: pending.id, result: '未执行：管理会话或权限失效'});
            // No file from a session that was just rejected: every card stays selected.
            if (action === 'export') for (const target of targets) keepSelected.add(target.id);
            codes.length = 0;
            break;
          }
        } finally {
          if (alive.current) setProgress({done: index + 1, total: targets.length});
        }
      }
      if (alive.current && action === 'export' && codes.length) {
        const url = URL.createObjectURL(new Blob([codes.join('\n')], {type: 'text/plain;charset=utf-8'}));
        try {
          const link = document.createElement('a');
          link.href = url;
          link.download = `selected-cards-${new Date().toISOString().slice(0, 10)}.txt`;
          link.click();
          succeeded = codes.length;
        } finally {setTimeout(() => URL.revokeObjectURL(url), 1000);}
      }
    } catch {
      for (const target of targets) keepSelected.add(target.id);
      succeeded = 0;
      if (alive.current) reportError('导出文件创建失败，未确认下载成功。请重新选择后重试。');
    } finally {
      codes.length = 0;
      if (alive.current) {
        for (const item of failures) keepSelected.add(item.id);
        const summary = action === 'export' ? `已开始下载 ${succeeded} 张卡密` : `${succeeded} 张已${verb}`;
        if (failures.length) setReport({summary: `${succeeded ? `${summary} · ` : ''}${failures.length} 张未完成`, failures});
        if (succeeded) toast.success(action === 'export' ? `${summary}，请核对下载文件` : summary);
        setSelectedIds([...keepSelected]);
        setProgress(null);
        await refresh({keepSelection: true});
        setBulkBusy(false);
      }
      writing.current = false;
    }
  };

  // ---- Reveal ----
  const reveal = async (card: AdminCardItem) => {
    if (revealing || bulkBusy) return;
    setRevealing(true); setRevealError('');
    try {
      const result = await adminApi.revealCard(card.id);
      if (!result.success || !result.rawCode) throw new Error('卡密不可恢复');
      if (alive.current) setRevealed({cardId: card.id, rawCode: result.rawCode});
    } catch (error) {
      reportError(`查看失败：${errorText(error)}`);
    } finally {if (alive.current) setRevealing(false);}
  };
  const copyRevealed = async () => {
    if (!revealed) return;
    try {await navigator.clipboard.writeText(revealed.rawCode); setRevealError(''); toast.success('已复制卡密');}
    catch {setRevealError('复制失败，请手动选中复制');}
  };

  // ---- Adjust ----
  const openAdjust = (card: AdminCardItem) => {
    if (bulkBusy || card.status === 'voided') return;
    if (!operator) {reportError('需要重新登录以确认操作人', reauthenticate); return;}
    try {
      const saved = loadAdjustment(sessionStorage, operator);
      if (saved && saved.cardId !== card.id) {reportError(`卡 ${saved.cardId} 的调账结果未确认，请先打开原卡核对。`); return;}
      reportError('');
      setIntent(saved); setAdjustCard(card); setAdjustStep('form');
      setAdjustDirection(saved && saved.delta < 0 ? 'deduct' : 'add');
      setAdjustAmount(saved ? String(Math.abs(saved.delta)) : '');
      setAdjustReason(saved?.reason ?? '');
    } catch {reportError('调账恢复记录无法读取，请人工核对账本；未发送请求。');}
  };
  const reviewPending = () => {
    try {
      if (!operator) return;
      const saved = loadAdjustment(sessionStorage, operator);
      if (!saved) {setIntent(null); reportError('未找到待核对的调账，请刷新列表。'); return;}
      const card = cards.find(item => item.id === saved.cardId);
      if (!card) {reportError('未读取到原卡记录，请刷新或人工核对账本；原意图仍保留。'); return;}
      reportError('');
      setIntent(saved); setAdjustCard(card); setAdjustStep('form');
      setAdjustDirection(saved.delta < 0 ? 'deduct' : 'add');
      setAdjustAmount(String(Math.abs(saved.delta))); setAdjustReason(saved.reason);
    } catch {reportError('调账恢复记录无法读取，请人工核对账本；未发送请求。');}
  };
  const closeAdjust = () => {if (!mutationBusy) {setAdjustCard(null); setAdjustStep('form');}};
  const locked = !!pendingIntent;
  const zeroMicro = !!pendingIntent && isZeroMicroAdjustment(pendingIntent);
  const parsedAdjust = ((): {delta: number} | {error: string} => {
    if (pendingIntent) return zeroMicro ? {error: '该旧意图换算为零微积分，无法入账'} : {delta: pendingIntent.delta};
    const text = adjustAmount.trim();
    if (!text) return {error: ''};
    if (!/^\d+(\.\d+)?$/.test(text)) return {error: '请输入不为 0 的正数（最多 6 位小数）'};
    try {
      const micro = adjustmentPointsToMicro(text);
      return {delta: (adjustDirection === 'deduct' ? -micro : micro) / 1_000_000};
    } catch {return {error: '请输入不为 0 的数量（最多 6 位小数，不超过 1,000,000）'};}
  })();
  const adjustDelta = 'delta' in parsedAdjust ? parsedAdjust.delta : null;
  const reasonText = adjustReason.trim();
  const reasonValid = !!reasonText && reasonText.length <= 500;
  const adjustReady = adjustDelta !== null && reasonValid;
  const adjustAfter = adjustCard && adjustDelta !== null ? adjustCard.pointsAvailable + adjustDelta : null;
  const signed = (value: number) => `${value > 0 ? '+' : ''}${formatCredits(value)}`;

  const submitAdjust = async () => {
    if (!adjustCard || mutationBusy || adjusting.current) return;
    if (!operator) {reportError('需要重新登录以确认操作人', reauthenticate); return;}
    if (adjustment.current && isZeroMicroAdjustment(adjustment.current)) {reportError('该旧意图换算为零微积分，无法入账；请清除后更正金额。'); return;}
    // Existing nonzero intents replay the original numeric payload, never rounded again.
    const delta = adjustment.current ? adjustment.current.delta : adjustDelta;
    if (delta === null) {reportError('请输入不为 0 的数量（最多 6 位小数），范围为 ±1,000,000；未发送请求。'); setAdjustStep('form'); return;}
    if (!reasonValid) {reportError('请填写 1–500 字的原因，不要包含密码或令牌'); setAdjustStep('form'); return;}
    let sent = false;
    try {
      adjusting.current = true; setMutationBusy(true); reportError('');
      const intent = loadAdjustment(sessionStorage, operator) ?? {operator, cardId: adjustCard.id, delta, reason: reasonText, key: crypto.randomUUID()};
      if (intent.cardId !== adjustCard.id || intent.delta !== delta || intent.reason !== reasonText) throw new Error('上次调账结果尚未确认，请先用原参数重试');
      saveAdjustment(sessionStorage, intent); setIntent(intent); sent = true;
      const response = await adminApi.adjustBalance(intent.cardId, intent.delta, intent.reason, intent.key);
      if (!response.success) throw new Error('服务端未确认调账');
      clearAdjustment(sessionStorage, intent); setIntent(null);
      toast.success(`已调整 ${shortId(adjustCard.id, 'card')}：${signed(intent.delta)} 积分`);
      setAdjustCard(null); setAdjustReason(''); setAdjustAmount(''); setAdjustStep('form');
      void refresh();
    } catch (error) {
      if (sent && adjustment.current && error instanceof AdminApiError && isUnsubmittedAdjustmentRejection(error.status, error.message, adjustment.current)) {
        try {
          clearAdjustment(sessionStorage, adjustment.current); setIntent(null); setAdjustStep('form');
          reportError('服务端明确拒绝调账，未入账；已清除该无效意图，请检查卡密状态、账面余额与调账金额。');
        } catch {reportError('该调账未入账，但本地记录未能清除，请检查浏览器存储。');}
        return;
      }
      reportError(sent ? `没收到调账结果（${errorText(error)}）。重新打开这张卡可按原参数重试。` : `尚未发送调账：${errorText(error)}。`);
    } finally {adjusting.current = false; setMutationBusy(false);}
  };
  const clearZeroMicro = async () => {
    const intent = adjustment.current;
    if (!intent) return;
    if (!(await confirmAction({title: '清除无效的调账意图？', facts: [`卡 ${shortId(intent.cardId, 'card')} · 原因：${intent.reason}`], consequence: '该意图换算为零微积分，无法入账；清除后可重新填写，不会自动提交。', confirmLabel: '清除'}))) return;
    try {clearAdjustment(sessionStorage, intent); setIntent(null); setAdjustAmount(''); reportError('无效意图已清除，请重新填写金额；未发送请求。');}
    catch {reportError('本地记录清除失败，请检查浏览器存储。');}
  };

  // ---- Issue ----
  const issuable = configFailed ? [] : groups.filter(group => group.issuance_enabled !== false);
  const issuableKey = issuable.map(group => String(group.id)).join('\n');
  useEffect(() => {
    const ids = issuableKey ? issuableKey.split('\n') : [];
    setBatchGroup(current => ids.includes(current) ? current : ids.length === 1 ? ids[0] : '');
  }, [issuableKey]);
  const countValue = Number(batchCount);
  const validCount = /^\d+$/.test(batchCount) && Number.isInteger(countValue) && countValue >= 1 && countValue <= 500;
  const tier = TIERS.find(item => item.id === batchTemplate);
  const selectedGroup = issuable.find(group => group.id === batchGroup);
  const note = batchNote.trim();
  const generateBlocked = !!issuanceRecovery ? '上次批量生成的结果未确认，请先核对' : !selectedGroup ? '请选择分组' : !validCount ? '请输入 1–500 的整数' : undefined;

  const refreshIssuanceForReview = async () => {
    if (issuanceChecking || writing.current) return;
    setIssuanceChecking(true); setIssuanceRecovery('refresh');
    try {
      const result = await adminApi.getCards();
      if (!result.success) throw new Error('服务端未确认卡密列表');
      if (!alive.current) return;
      updateCards(result.cards); setIssuanceRecovery('review'); reportError('');
    } catch {reportError('卡密核对刷新失败，仍禁止制卡，请重试。');}
    finally {if (alive.current) setIssuanceChecking(false);}
  };
  const releaseIssuance = async () => {
    if (!(await confirmAction({title: '确认已核对这批卡？', consequence: '如果这批卡已经生成，请不要重复生成。解除后不会自动生成。', confirmLabel: '继续制卡'}))) return;
    try {sessionStorage.removeItem(ISSUANCE_KEY); setIssuanceRecovery(null); reportError('');}
    catch {reportError('无法清除待核对记录，仍禁止制卡。请检查浏览器存储。');}
  };

  const generate = async () => {
    if (issuanceRecovery || writing.current || loading || generating || !tier || !selectedGroup || !validCount) return;
    const confirmed = await confirmAction({
      title: `生成 ${countValue} 张 ${tier.name}（${formatCount(tier.points)} 积分）卡密？`,
      facts: [`套餐：${tier.name} · ${formatCount(tier.points)} 积分`, `分组：${String(selectedGroup.name ?? selectedGroup.id)}`,
        `合计 ${formatCount(tier.points * countValue)} 积分 · 有效期 30 天（激活起算）· 每张 1 台设备`, ...(note ? [`备注：${note}`] : [])],
      confirmLabel: `生成 ${countValue} 张`,
    });
    if (!confirmed || !alive.current || writing.current || issuanceRecovery) return;
    try {
      writing.current = true; setGenerating(true);
      const reference = `批次 ${new Date().toISOString()} ${crypto.randomUUID()}`;
      setIssuanceReference(reference);
      sessionStorage.setItem(ISSUANCE_KEY, JSON.stringify({reference, count: countValue, group: batchGroup, template: batchTemplate, startedAt: new Date().toISOString()}));
      // The batch reference stays in the note, so an unconfirmed batch can still be found.
      const response = await adminApi.batchCards(countValue, batchGroup, batchTemplate, note ? `${note} · ${reference}` : reference);
      if (!response.success) throw new Error('服务器未确认生成结果');
      sessionStorage.removeItem(ISSUANCE_KEY);
      toast.success(`已生成 ${response.cards.length} 张卡密`);
      setGenerated({cards: response.cards, tier: tier.name, points: tier.points, groupName: String(selectedGroup.name ?? selectedGroup.id)});
      setGeneratedError(''); setShowBatch(false); setBatchNote('');
      await refresh();
    } catch (error) {
      if (refused(error)) {sessionStorage.removeItem(ISSUANCE_KEY); reportError(`服务端已拒绝，未生成卡密：${errorText(error)}`); return;}
      setIssuanceRecovery('refresh'); setShowBatch(false);
      reportError(`没收到生成结果（${errorText(error)}），请先核对列表，不要重复生成。`);
    } finally {writing.current = false; if (alive.current) setGenerating(false);}
  };

  const downloadGenerated = () => {
    if (!generated?.cards.length) return;
    const cell = (value: string | number) => {
      const text = String(value);
      return `"${(/^[=+@\-\t\r]/.test(text) ? "'" + text : text).replace(/"/g, '""')}"`;
    };
    const csv = ['cardId,rawCode,groupId,creditTotal', ...generated.cards.map(card =>
      [card.cardId, card.rawCode, card.groupId, card.creditTotal].map(cell).join(','))].join('\r\n');
    const url = URL.createObjectURL(new Blob(['\uFEFF' + csv], {type: 'text/csv;charset=utf-8'}));
    const link = document.createElement('a');
    link.href = url;
    link.download = `generated-cards-${new Date().toISOString().slice(0, 10)}.csv`;
    document.body.appendChild(link);
    link.click();
    link.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  };
  const copyGenerated = async (text: string, done: string) => {
    try {await navigator.clipboard.writeText(text); setGeneratedError(''); toast.success(done);}
    catch {setGeneratedError('复制失败，请使用 CSV 下载');}
  };
  const finishGenerated = async () => {
    if (await confirmAction({title: '已保存这些卡密？', consequence: '关闭后不再显示明文。', confirmLabel: '完成'})) {
      if (alive.current) setGenerated(null);
    }
  };

  const toggleSort = (key: 'balance' | 'expiry') => setSort(current => current?.key !== key ? {key, direction: -1} : current.direction === -1 ? {key, direction: 1} : null);
  const sortIcon = (key: 'balance' | 'expiry') => sort?.key === key ? (sort.direction === 1 ? <IconSortUp/> : <IconSortDown/>) : null;
  const sortState = (key: 'balance' | 'expiry') => sort?.key === key ? (sort.direction === 1 ? 'ascending' : 'descending') : undefined;
  const allSelected = pageCards.length > 0 && pageCards.every(card => selectedIds.includes(card.id));
  const someSelected = selectedIds.length > 0;
  const bulkDisabled = bulkBusy || failed || loading || mutationBusy || revealing;

  return <div className="page-stack">
    <TopbarActions><button type="button" className="btn btn-primary" onClick={() => {reportError(''); setShowBatch(true);}}>＋ 批量生成</button></TopbarActions>

    {issuanceRecovery && <section className="recovery-panel" aria-label="制卡结果核对">
      <div>
        <h3>上次批量生成的结果未确认</h3>
        <p>可能已经生成。核对列表后再继续制卡。{issuanceReference && <span className="muted" title={issuanceReference}> {formatBatchNote(issuanceReference)}</span>}</p>
      </div>
      <div className="button-row">
        <button type="button" className="btn btn-small" disabled={!issuanceReference} onClick={() => {setSearch(issuanceReference); setGroupFilter('ALL'); setStatusTab('ALL'); setQuick(null);}}>查看这批卡</button>
        <button type="button" className="btn btn-small" disabled={issuanceChecking || loading} onClick={() => void refreshIssuanceForReview()}>{issuanceChecking ? '正在刷新…' : '刷新列表'}</button>
        <button type="button" className="btn btn-small" disabled={issuanceChecking || issuanceRecovery !== 'review'} title={issuanceRecovery !== 'review' ? '先刷新列表' : undefined} onClick={() => void releaseIssuance()}>已核对，继续制卡</button>
      </div>
    </section>}

    {pendingIntent && <section className="recovery-panel" aria-label="未确认调账恢复">
      <p>卡 <IdCell value={pendingIntent.cardId} kind="card"/> 有一笔调账结果未确认</p>
      <button type="button" className="btn btn-small" disabled={bulkBusy || mutationBusy || loading} onClick={reviewPending}>核对</button>
    </section>}

    <div className="toolbar">
      <div className="filter-bar" role="search" aria-label="卡密筛选">
        <label className="search-field"><IconSearch/>
          <input type="text" aria-label="搜索卡密" placeholder="卡密 ID、备注或设备 ID" value={search} disabled={bulkBusy} onChange={event => setSearch(event.target.value)}/>
        </label>
        <label className="inline-field"><span>分组</span>
          <select aria-label="分组筛选" value={groupFilter} disabled={bulkBusy} onChange={event => setGroupFilter(event.target.value)}>
            <option value="ALL">全部</option>
            {Array.from(new Set(cards.map(card => card.groupId))).map(id => {
              const name = groupName(id);
              const repeated = groups.filter(group => group.name === name).length > 1;
              return <option key={id} value={id}>{repeated || name === id ? `${name} · ${id}` : name}</option>;
            })}
          </select>
        </label>
        <div className="chips" aria-label="快捷筛选" role="group">
          <button type="button" className="chip" aria-pressed={quick === 'expiring'} disabled={bulkBusy} onClick={() => setQuick(value => value === 'expiring' ? null : 'expiring')}>7 天内到期 <span className="chip-count">{quickCount('expiring')}</span></button>
          <button type="button" className="chip" aria-pressed={quick === 'low'} disabled={bulkBusy} onClick={() => setQuick(value => value === 'low' ? null : 'low')}>余额低于 10% <span className="chip-count">{quickCount('low')}</span></button>
        </div>
      </div>
      <FilterTabs label="状态筛选" value={statusTab} options={tabs} onChange={setStatusTab} disabled={bulkBusy}/>
    </div>

    {filtersActive && filtered.length > 0 && <p className="filter-summary">筛选出 {formatCount(filtered.length)} 张{failed ? '（可能不是最新）' : ''}
      <button type="button" className="btn-text" disabled={bulkBusy} onClick={resetFilters}>清除筛选</button></p>}

    {someSelected && <div className="selection-bar" role="region" aria-label="批量卡密管理">
      <span className="selection-count">{progress ? `处理中 ${progress.done}/${progress.total}` : `已选 ${selectedIds.length} 张`}</span>
      <div className="button-row">
        <button type="button" className="btn btn-small" disabled={bulkDisabled} onClick={() => void runBulk('freeze')}>冻结</button>
        <button type="button" className="btn btn-small" disabled={bulkDisabled} onClick={() => void runBulk('unfreeze')}>解冻</button>
        <button type="button" className="btn btn-small" disabled={bulkDisabled} onClick={() => void runBulk('export')}>导出明文</button>
        <Menu label="更多批量操作" className="btn btn-small" disabled={bulkDisabled} items={[
          {label: '归档', onSelect: () => void runBulk('archive')},
          {label: '取消归档', onSelect: () => void runBulk('unarchive')},
          {label: '封禁', danger: true, onSelect: () => void runBulk('ban')},
        ]}>更多<IconChevronDown/></Menu>
        <button type="button" className="btn btn-small btn-danger" disabled={bulkDisabled} onClick={() => void runBulk('void')}>永久作废</button>
      </div>
      <button type="button" className="btn-text" disabled={bulkBusy} onClick={() => setSelectedIds([])}>取消选择</button>
    </div>}

    {report && <div className="bulk-report" role="status" aria-label="批量操作结果">
      <div className="bulk-report-head"><b>{report.summary}</b>
        <button type="button" className="btn-icon" aria-label="关闭结果" title="关闭" onClick={() => setReport(null)}><IconClose/></button></div>
      <ul>{report.failures.map(item => <li key={item.id}>{item.id}：{item.result}</li>)}</ul>
    </div>}

    <div className="table-card">
      <div className="table-scroll"><table className="table cards-table">
        <thead><tr>
          <th className="col-check"><input type="checkbox" aria-label="全选本页" checked={allSelected} disabled={bulkBusy || failed || loading || !pageCards.length}
            ref={element => {if (element) element.indeterminate = someSelected && !allSelected;}}
            onChange={event => setSelectedIds(event.target.checked ? pageCards.map(card => card.id) : [])}/></th>
          <th>卡密</th><th>备注</th><th>分组</th>
          <th className="num" aria-sort={sortState('balance')}><button type="button" className="th-sort" onClick={() => toggleSort('balance')}>余额{sortIcon('balance')}</button></th>
          <th aria-sort={sortState('expiry')}><button type="button" className="th-sort" onClick={() => toggleSort('expiry')}>到期{sortIcon('expiry')}</button></th>
          <th className="col-device">设备</th><th className="col-status">状态</th><th className="col-actions"><span className="sr-only">操作</span></th>
        </tr></thead>
        <tbody>
          {pageCards.map(card => {
            const selected = selectedIds.includes(card.id);
            const menuItems: MenuItem[] = [
              ...(card.status === 'active' ? [{label: '冻结', onSelect: () => void changeStatus(card, 'freeze')}] : []),
              ...(card.status === 'frozen' ? [{label: '解冻', onSelect: () => void changeStatus(card, 'unfreeze')}] : []),
              ...(!['banned', 'voided'].includes(card.status) ? [{label: '封禁', danger: true, onSelect: () => void changeStatus(card, 'ban')}] : []),
            ];
            return <tr key={card.id} className={selected ? 'is-selected' : undefined}>
              <td className="col-check"><input type="checkbox" aria-label={`选择卡密 ${card.id}`} disabled={bulkBusy || failed || loading} checked={selected}
                onChange={event => {const checked = event.currentTarget.checked; setSelectedIds(ids => checked ? [...ids, card.id] : ids.filter(id => id !== card.id));}}/></td>
              <td className="col-id"><IdCell value={card.id} kind="card"/></td>
              <td className="col-note" title={card.note || undefined}>{card.note ? <span className="clip clip-note">{formatBatchNote(card.note)}</span> : <span className="muted">—</span>}</td>
              <td className="col-group" title={card.groupId}><span className="clip clip-group">{groupName(card.groupId)}</span></td>
              <td className="num"><Balance card={card}/></td>
              <td className="col-expiry"><Expiry card={card} now={now}/></td>
              <td className="col-device"><Devices card={card}/></td>
              <td className="col-status"><span className="status-cell"><StatusBadge view={cardStatusView(card.status)}/>{card.archivedAt != null && <Tag>已归档</Tag>}</span></td>
              <td className="col-actions"><span className="row-actions">
                <button type="button" className="btn-text" disabled={!card.codeRecoverable || revealing || bulkBusy} aria-label="查看卡密" title={card.codeRecoverable ? undefined : '此卡未保存明文'} onClick={() => void reveal(card)}>查看</button>
                <button type="button" className="btn-text" disabled={card.status === 'voided' || blocked} title={card.status === 'voided' ? '已作废，不能调账' : staleTitle} onClick={() => openAdjust(card)}>调账</button>
                {menuItems.length ? <Menu label="更多操作" items={menuItems} disabled={blocked} title={staleTitle ?? '更多操作'}/> : <span className="menu-placeholder"/>}
              </span></td>
            </tr>;
          })}
          {!filtered.length && <TableState colSpan={9} loading={loading} failed={failed && !cards.length} onRetry={() => void refresh()}
            empty={!cards.length ? '还没有卡密' : '没有匹配的卡密'}
            action={!cards.length ? <button type="button" className="btn btn-small" onClick={() => setShowBatch(true)}>批量生成</button> : <span className="button-row">
              {filtersActive && (search.trim() || groupFilter !== 'ALL' || quick) && <button type="button" className="btn btn-small" disabled={bulkBusy} onClick={resetFilters}>清除筛选</button>}
              {statusTab !== 'ALL' && <button type="button" className="btn btn-small" disabled={bulkBusy} onClick={() => {setStatusTab('ALL');}}>查看全部</button>}
            </span>}/>}
        </tbody>
      </table></div>
      <Pager page={currentPage} pageSize={PAGE_SIZE} total={filtered.length} disabled={bulkBusy || failed || loading} onPage={next => {setSelectedIds([]); setPage(next);}}/>
    </div>

    {revealed && <Modal label="查看卡密" onClose={() => {setRevealed(null); setRevealError('');}} className="dialog-narrow">
      <h3 className="modal-title">卡密 <span className="mono">{shortId(revealed.cardId, 'card')}</span></h3>
      <textarea aria-label="卡密明文" readOnly rows={2} className="secret-block" value={revealed.rawCode} onFocus={event => event.currentTarget.select()}/>
      <p className="muted">关闭后不再显示。</p>
      {revealError && <p role="alert" className="form-error">{revealError}</p>}
      <div className="modal-actions">
        <button type="button" className="btn" onClick={() => {setRevealed(null); setRevealError('');}}>关闭</button>
        <button type="button" className="btn btn-primary" data-autofocus onClick={() => void copyRevealed()}>复制</button>
      </div>
    </Modal>}

    {generated && <Modal label="新生成的卡密" onClose={() => void finishGenerated()} className="dialog-wide">
      <h3 className="modal-title">已生成 {generated.cards.length} 张 · {generated.tier} {formatCount(generated.points)} 积分</h3>
      <p className="note-warning">关闭后不再显示明文，请先下载或复制。分组：{generated.groupName}</p>
      {generatedError && <p role="alert" className="form-error">{generatedError}</p>}
      <div className="table-scroll generated-list"><table className="table table-compact">
        <thead><tr><th>卡密 ID</th><th>卡密</th><th className="col-actions"><span className="sr-only">操作</span></th></tr></thead>
        <tbody>{generated.cards.map(card => <tr key={card.cardId}>
          <td className="mono">{card.cardId}</td><td className="mono secret">{card.rawCode}</td>
          <td className="col-actions"><button type="button" className="btn-text" onClick={() => void copyGenerated(card.rawCode, '已复制该卡密')}>复制</button></td>
        </tr>)}</tbody>
      </table></div>
      <div className="modal-actions">
        <button type="button" className="btn" onClick={() => void copyGenerated(generated.cards.map(card => card.rawCode).join('\n'), '已复制全部卡密')}>复制全部</button>
        <button type="button" className="btn btn-primary" onClick={downloadGenerated}>下载 CSV</button>
        <button type="button" className="btn" onClick={() => void finishGenerated()}>完成</button>
      </div>
    </Modal>}

    {showBatch && <Modal label="批量生成卡密" onClose={() => setShowBatch(false)} busy={generating} className="dialog-form">
      <h3 className="modal-title">批量生成卡密</h3>
      {actionError && <p role="alert" className="form-error">{actionError}</p>}
      <div className="form-grid">
        <label className="field"><span className="field-label">套餐</span>
          <select aria-label="积分套餐" value={batchTemplate} disabled={generating} onChange={event => setBatchTemplate(event.target.value)}>
            {TIERS.map(item => <option key={item.id} value={item.id}>{item.name} · {formatCount(item.points)} 积分 · ¥{item.price_cny}</option>)}
          </select></label>
        <label className="field"><span className="field-label">分组</span>
          <select aria-label="模型与计费分组" value={batchGroup} disabled={generating || !issuable.length} onChange={event => setBatchGroup(event.target.value)}>
            <option value="" disabled>{issuable.length ? '请选择分组' : '没有可发卡的分组'}</option>
            {issuable.map(group => <option key={String(group.id)} value={String(group.id)}>{String(group.name ?? group.id)}</option>)}
          </select>
          {!issuable.length && <span className="field-hint">{configFailed ? '分组配置没有加载成功，请刷新后再试' : '在“分组与权益”中开启“可发新卡”'}</span>}
        </label>
        <label className="field"><span className="field-label">数量</span>
          <input type="number" aria-label="生成数量" aria-invalid={!validCount} aria-describedby="batch-count-help" min="1" max="500" step="1"
            value={batchCount} disabled={generating} onChange={event => setBatchCount(event.target.value)}/>
          {!validCount && <span id="batch-count-help" className="field-error">请输入 1–500 的整数</span>}
        </label>
        <label className="field"><span className="field-label">备注</span>
          <input aria-label="备注" maxLength={60} placeholder="例：淘宝 9 月 / 客户张三（可选）" value={batchNote} disabled={generating} onChange={event => setBatchNote(event.target.value)}/></label>
      </div>
      <p className="batch-summary" aria-label="发卡摘要">合计 {validCount && tier ? formatCount(tier.points * countValue) : '—'} 积分 · 面值 {validCount && tier ? formatMoney(tier.price_cny * countValue * 1_000_000) : '—'} · 每张 1 台设备 · 有效期 30 天（激活起算）</p>
      <div className="modal-actions">
        <button type="button" className="btn" disabled={generating} onClick={() => setShowBatch(false)}>取消</button>
        <button type="button" className="btn btn-primary" disabled={!!generateBlocked || generating || loading} title={generateBlocked} onClick={() => void generate()}>
          {generating ? '生成中…' : validCount ? `生成 ${countValue} 张` : '生成'}</button>
      </div>
    </Modal>}

    {adjustCard && <Modal label="卡密调账" onClose={closeAdjust} busy={mutationBusy} className="dialog-form">
      <h3 className="modal-title">调整积分 · <span className="mono">{shortId(adjustCard.id, 'card')}</span></h3>
      {actionError && <p role="alert" className="form-error">{actionError}</p>}
      {pendingIntent && !zeroMicro && <p className="note-warning">上次结果未确认，将按原参数重试</p>}
      {zeroMicro && <div role="alert" className="form-error">
        <p>旧意图金额换算为零微积分，无法入账。清除后可重新填写，不会自动提交。</p>
        <button type="button" className="btn btn-small" disabled={mutationBusy} onClick={() => void clearZeroMicro()}>清除零微积分意图</button>
      </div>}
      <p className="adjust-balance">当前余额 <b>{formatCredits(adjustCard.pointsAvailable)}</b> 积分</p>
      <div className="adjust-row">
        <div className="segmented" role="radiogroup" aria-label="调账方向">
          {(['add', 'deduct'] as const).map(direction => <button key={direction} type="button" role="radio" aria-checked={adjustDirection === direction}
            disabled={mutationBusy || locked || adjustStep === 'review'} onClick={() => setAdjustDirection(direction)}>{direction === 'add' ? '增加' : '扣减'}</button>)}
        </div>
        <input type="number" step="any" min="0" aria-label="增减积分数量" aria-invalid={'error' in parsedAdjust && !!parsedAdjust.error}
          placeholder="数量" disabled={mutationBusy || locked || adjustStep === 'review'} value={adjustAmount} onChange={event => setAdjustAmount(event.target.value)}/>
        <span className="adjust-result">→ 调整后 <b>{adjustAfter === null ? '—' : formatCredits(adjustAfter)}</b> 积分</span>
      </div>
      {'error' in parsedAdjust && parsedAdjust.error && !zeroMicro && <p className="field-error">{parsedAdjust.error}</p>}
      {adjustAfter !== null && adjustAfter < 0 && <p className="field-warning">余额不足：调整后为负数，服务器会拒绝</p>}
      <label className="field"><span className="field-label">原因</span>
        <input type="text" aria-label="调账原因说明" maxLength={500} placeholder="例：补偿 9/25 上游中断"
          disabled={mutationBusy || locked || adjustStep === 'review'} value={adjustReason} onChange={event => setAdjustReason(event.target.value)}/></label>
      {adjustStep === 'review' && adjustDelta !== null && <div className="review-box" aria-label="调账复核">
        <p><span className="mono">{shortId(adjustCard.id, 'card')}</span>：{formatCredits(adjustCard.pointsAvailable)} → <b>{formatCredits(adjustCard.pointsAvailable + adjustDelta)}</b>（{signed(adjustDelta)}）</p>
        <p>原因：{reasonText}</p>
      </div>}
      <div className="modal-actions">
        {adjustStep === 'form' ? <>
          <button type="button" className="btn" disabled={mutationBusy} onClick={closeAdjust}>取消</button>
          <button type="button" className="btn btn-primary" disabled={!adjustReady || mutationBusy || zeroMicro}
            title={adjustReady ? undefined : !reasonValid && adjustDelta !== null ? '填写原因后继续' : '填写数量和原因后继续'} onClick={() => setAdjustStep('review')}>下一步</button>
        </> : <>
          <button type="button" className="btn" disabled={mutationBusy} onClick={() => setAdjustStep('form')}>返回修改</button>
          <button type="button" className="btn btn-primary" disabled={mutationBusy} onClick={() => void submitAdjust()}>{mutationBusy ? '提交中…' : '确认入账'}</button>
        </>}
      </div>
    </Modal>}
  </div>;
}

export {TIERS};
