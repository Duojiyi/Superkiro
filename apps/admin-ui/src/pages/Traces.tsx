// 调用追踪: one line per request with the columns needed to find a problem, and details in
// a drawer beside the list. Request content is read only when a content tab is opened.
import {useEffect, useRef, useState} from 'react';
import {adminApi, AdminApiError, type AdminCardItem, type AdminTrace, type TraceContent, type TraceReply} from '../api';
import {confirmAction} from '../components/confirm';
import {IconChevronDown, IconChevronUp, IconClose, IconCopy, IconDoc, IconSearch, IconSortDown, IconSortUp} from '../components/icons';
import {Menu} from '../components/menu';
import {Drawer, isModalOpen} from '../components/modal';
import {toast} from '../components/toast';
import {FilterTabs, IdCell, Pager, StatusBadge, TableState, TopbarActions, copyText, type TabOption} from '../components/ui';
import {formatCharge, formatClock, formatCount, formatDateTime, formatDuration, formatFullDateTime, formatListTime, formatMoney, formatRelative, formatShortDate, formatSpeed, formatTokenCount} from '../format';
import {errorClassLabel, TRACE_IN_PROGRESS, traceStatusView} from '../status';
import type {Refresh, ReportError, TraceTab, TraceWindow, WriteGuards} from '../types';
import {ConversationView, RawView, ReplyView} from './TraceContent';

const PAGE_SIZE = 50;
const RETENTION = 86400;
type SortKey = 'time' | 'ttft' | 'charge';

const latency = (trace: AdminTrace) => (trace.attempt_chain ?? []).reduce((sum, attempt) => sum + Number(attempt.latency_ms ?? 0), 0);
const statusMatches = (trace: AdminTrace, tab: TraceTab) =>
  tab === 'ALL' || (tab === 'in_progress' ? TRACE_IN_PROGRESS.includes(String(trace.status)) : trace.status === tab);
const ttftTone = (ms: unknown) => typeof ms === 'number' ? (ms > 15000 ? 'is-danger' : ms > 5000 ? 'is-warning' : '') : '';
const errorText = (error: unknown) => (error instanceof Error ? error.message : String(error));

export default function TracesPage({traces, cards, loading, failed, refresh, guards, reportError, intent, onOpenCard}: {
  traces: AdminTrace[];
  cards: AdminCardItem[];
  loading: boolean;
  failed: boolean;
  refresh: Refresh;
  guards: WriteGuards;
  reportError: ReportError;
  intent?: {status?: TraceTab; window?: TraceWindow; search?: string; open?: string};
  onOpenCard: (cardId: string) => void;
}) {
  const {writing} = guards;
  const alive = useRef(true);
  useEffect(() => {alive.current = true; return () => {alive.current = false;};}, []);
  // Arriving from a card's details: that card's requests, with the one clicked already open.
  const [query, setQuery] = useState(intent?.search ?? '');
  const [status, setStatus] = useState<TraceTab>(intent?.status ?? 'ALL');
  const [model, setModel] = useState('ALL');
  const [provider, setProvider] = useState('ALL');
  // Under 失败: one failure reason at a time.
  const [reason, setReason] = useState<string | null>(null);
  const [range, setRange] = useState<TraceWindow>(intent?.window ?? 'all');
  const [sort, setSort] = useState<{key: SortKey; direction: 1 | -1}>({key: 'time', direction: -1});
  const [page, setPage] = useState(0);
  const [selectedId, setSelectedId] = useState<string | null>(intent?.open ?? null);
  const [focusToken, setFocusToken] = useState(0);
  const [followToken, setFollowToken] = useState(0);
  const [pruning, setPruning] = useState(false);

  // A full card ID in the search box asks the server for that card's own latest requests,
  // beyond the 500 kept in the global list.
  const exactCard = cards.some(card => card.id === query.trim()) ? query.trim() : null;
  const [cardScope, setCardScope] = useState<{cardId: string; traces: AdminTrace[] | null; failed?: boolean} | null>(null);
  useEffect(() => {
    if (!exactCard) {setCardScope(null); return;}
    let current = true;
    setCardScope({cardId: exactCard, traces: null});
    adminApi.getTraces(500, exactCard).then(result => {
      if (!current) return;
      if (result.success !== true) throw new Error('服务器未确认读取成功');
      setCardScope({cardId: exactCard, traces: result.traces});
    }).catch(() => {if (current) setCardScope({cardId: exactCard, traces: null, failed: true});});
    return () => {current = false;};
  }, [exactCard, traces]);
  const scopedToCard = !!cardScope && cardScope.cardId === exactCard && !!cardScope.traces;
  const source = scopedToCard ? cardScope!.traces! : traces;

  useEffect(() => {setPage(0);}, [query, status, model, provider, reason, range, sort]);
  useEffect(() => {if (status !== 'error') setReason(null);}, [status]);

  const nowSecs = Date.now() / 1000;
  const text = query.trim().toLowerCase();
  const scoped = source.filter(trace =>
    (range === 'all' || Number(trace.ts) > nowSecs - (range === 'hour' ? 3600 : RETENTION)) &&
    (model === 'ALL' || trace.exposed_model === model) &&
    (provider === 'ALL' || trace.provider_id === provider) &&
    (!text || [trace.id, trace.invocation_id, trace.card_id, trace.exposed_model].some(value => String(value ?? '').toLowerCase().includes(text))));
  const filtered = scoped.filter(trace => statusMatches(trace, status) && (reason === null || String(trace.error_class ?? '') === reason));
  // Failure reasons among the failed requests in view, most frequent first.
  const reasonCounts = [...scoped.filter(trace => trace.status === 'error').reduce((counts, trace) => {
    const key = String(trace.error_class ?? '');
    return counts.set(key, (counts.get(key) ?? 0) + 1);
  }, new Map<string, number>())].sort((a, b) => b[1] - a[1]);
  const sortValue = (trace: AdminTrace): number | null => sort.key === 'time' ? Number(trace.ts)
    : sort.key === 'ttft' ? (typeof trace.ttft_ms === 'number' ? trace.ttft_ms : null)
    : Number(trace.credits_charged ?? 0);
  filtered.sort((a, b) => {
    const left = sortValue(a), right = sortValue(b);
    if (left === null || right === null) return left === null ? (right === null ? 0 : 1) : -1;
    return (left - right) * sort.direction;
  });
  const pageCount = Math.max(1, Math.ceil(filtered.length / PAGE_SIZE));
  const currentPage = Math.min(page, pageCount - 1);
  const rows = filtered.slice(currentPage * PAGE_SIZE, (currentPage + 1) * PAGE_SIZE);
  const selectedIndex = selectedId ? filtered.findIndex(trace => trace.id === selectedId) : -1;
  const selected = selectedIndex >= 0 ? filtered[selectedIndex] : selectedId ? source.find(trace => trace.id === selectedId) ?? null : null;
  const filtersActive = !!text || status !== 'ALL' || model !== 'ALL' || provider !== 'ALL' || reason !== null || range !== 'all';
  const resetFilters = () => {setQuery(''); setStatus('ALL'); setModel('ALL'); setProvider('ALL'); setReason(null); setRange('all'); setPage(0);};
  const providers = Array.from(new Set(source.map(trace => String(trace.provider_id ?? '')).filter(Boolean))).sort();
  const models = Array.from(new Set(source.map(trace => String(trace.exposed_model ?? '')).filter(Boolean))).sort();
  const oldest = source.length ? Math.min(...source.map(trace => Number(trace.ts))) : null;
  const tabs: TabOption<TraceTab>[] = ([['ALL', '全部'], ['error', '失败'], ['client_aborted', '中断'], ['in_progress', '进行中'], ['success', '成功']] as Array<[TraceTab, string]>)
    .map(([value, label]) => ({value, label, count: scoped.filter(trace => statusMatches(trace, value)).length, tone: value === 'error' ? 'danger' : value === 'client_aborted' ? 'warning' : undefined}));

  const open = (trace: AdminTrace) => {setSelectedId(trace.id); setFocusToken(token => token + 1);};
  const move = (step: number) => {
    if (selectedIndex < 0) return;
    const next = filtered[selectedIndex + step];
    if (!next) return;
    setSelectedId(next.id);
    setPage(Math.floor((selectedIndex + step) / PAGE_SIZE));
    setFollowToken(token => token + 1);
  };
  // The selected row stays in view while ↑/↓ move through the list.
  useEffect(() => {if (followToken) document.querySelector('.traces-table tr.is-selected')?.scrollIntoView({block: 'nearest'});}, [followToken]);
  useEffect(() => {if (focusToken) document.getElementById('trace-detail')?.focus({preventScroll: true});}, [focusToken]);
  useEffect(() => {
    if (!selectedId) return;
    const keydown = (event: KeyboardEvent) => {
      if ((event.key !== 'ArrowDown' && event.key !== 'ArrowUp') || event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey || isModalOpen()) return;
      const target = event.target instanceof HTMLElement ? event.target : null;
      if (target?.closest('input, textarea, select, [contenteditable="true"], [role="tablist"], [role="menu"], .drawer-content')) return;
      event.preventDefault();
      move(event.key === 'ArrowDown' ? 1 : -1);
    };
    document.addEventListener('keydown', keydown);
    return () => document.removeEventListener('keydown', keydown);
  });

  const toggleSort = (key: SortKey) => setSort(current => current.key === key ? {key, direction: current.direction === -1 ? 1 : -1} : {key, direction: -1});
  const sortIcon = (key: SortKey) => sort.key === key ? (sort.direction === 1 ? <IconSortUp/> : <IconSortDown/>) : null;
  const ariaSort = (key: SortKey) => sort.key === key ? (sort.direction === 1 ? 'ascending' : 'descending') : undefined;

  const prune = async () => {
    if (writing.current || pruning) return;
    const cutoff = Math.floor(Date.now() / 1000) - 30 * 86400;
    const confirmed = await confirmAction({
      title: '永久删除 30 天前的调用记录？',
      facts: [`删除 ${formatFullDateTime(cutoff)} 之前的记录`],
      consequence: '删除后无法恢复，请先完成审计与备份。',
      confirmLabel: '删除', danger: true, typed: '删除',
    });
    if (!confirmed || !alive.current || writing.current) return;
    writing.current = true; setPruning(true); reportError('');
    try {
      const result = await adminApi.pruneTraces(Math.floor(Date.now() / 1000) - 30 * 86400);
      if (!result.success) throw new Error('服务端未确认清理结果');
      toast.success(`已删除 ${result.pruned} 条记录`);
      await refresh();
    } catch (error) {
      reportError(`清理结果未确认：${errorText(error)}。请刷新核对，勿重复提交。`);
    } finally {writing.current = false; if (alive.current) setPruning(false);}
  };

  return <div className={`page-stack${selected ? ' has-drawer' : ''}`}>
    <TopbarActions>
      {pruning && <span className="busy-chip" role="status">正在清理…</span>}
      <Menu label="追踪更多操作" className="btn btn-icon-only" items={[{label: pruning ? '正在清理…' : '清理 30 天前的记录', danger: true, disabled: pruning, onSelect: () => void prune()}]}/>
    </TopbarActions>

    <div className="toolbar">
      <div className="filter-bar" role="search" aria-label="追踪筛选">
        <label className="search-field"><IconSearch/>
          <input type="text" aria-label="搜索调用记录" placeholder="卡密 ID 或请求 ID" value={query} onChange={event => setQuery(event.target.value)}/>
        </label>
        <label className="inline-field"><span>模型</span>
          <select aria-label="模型筛选" value={model} onChange={event => setModel(event.target.value)}>
            <option value="ALL">全部</option>
            {models.map(name => <option key={name} value={name}>{name}</option>)}
          </select>
        </label>
        {providers.length > 1 && <label className="inline-field"><span>供应商</span>
          <select aria-label="供应商筛选" value={provider} onChange={event => setProvider(event.target.value)}>
            <option value="ALL">全部</option>
            {providers.map(name => <option key={name} value={name}>{name}</option>)}
          </select>
        </label>}
        <div className="chips" role="group" aria-label="时间范围">
          {([['hour', '近 1 小时'], ['day', '近 24 小时'], ['all', '全部']] as Array<[TraceWindow, string]>).map(([value, label]) =>
            <button key={value} type="button" className="chip" aria-pressed={range === value} onClick={() => setRange(value)}>{label}</button>)}
        </div>
      </div>
      <FilterTabs label="追踪状态筛选" value={status} options={tabs} onChange={setStatus}/>
      {status === 'error' && reasonCounts.length > 0 && <div className="chips reason-chips" role="group" aria-label="失败原因">
        {reasonCounts.map(([key, count]) => <button key={key || 'none'} type="button" className="chip" aria-pressed={reason === key}
          onClick={() => setReason(current => current === key ? null : key)}>{key ? errorClassLabel(key) : '未分类'} <span className="chip-count">{count}</span></button>)}
      </div>}
    </div>

    <p className="filter-summary">
      {scopedToCard ? `这张卡最近 ${formatCount(source.length)} 次请求` : `最近 ${formatCount(source.length)} 次请求${oldest ? `（${formatShortDate(oldest)} ${formatClock(oldest)} 起）` : ''}`}
      {cardScope?.failed && <span className="is-warning"> · 没能按这张卡查询，显示的是最近的请求</span>}
      {filtersActive && <> · 匹配 {formatCount(filtered.length)} 条{filtered.length > 0 && <button type="button" className="btn-text" onClick={resetFilters}>清除筛选</button>}</>}
    </p>

    <div className="table-card">
      <div className="table-scroll"><table className="table traces-table">
        <caption className="sr-only">调用记录</caption>
        <thead><tr>
          <th aria-sort={ariaSort('time')}><button type="button" className="th-sort" onClick={() => toggleSort('time')}>时间{sortIcon('time')}</button></th>
          <th>卡密</th><th>模型</th><th>结果</th>
          <th className="num" aria-sort={ariaSort('ttft')}><button type="button" className="th-sort" onClick={() => toggleSort('ttft')}>首字{sortIcon('ttft')}</button></th>
          <th className="num col-speed">速度</th><th className="num col-tokens">Tokens 入 / 出</th><th className="num">耗时</th>
          <th className="num" aria-sort={ariaSort('charge')}><button type="button" className="th-sort" onClick={() => toggleSort('charge')}>扣费{sortIcon('charge')}</button></th>
          <th className="col-actions"><span className="sr-only">操作</span></th>
        </tr></thead>
        <tbody>
          {rows.map(trace => {
            const view = traceStatusView(trace.status);
            const attempts = trace.attempt_chain?.length ?? 0;
            const recent = nowSecs - Number(trace.ts) < RETENTION;
            return <tr key={trace.id} className={`is-clickable${trace.id === selectedId ? ' is-selected' : ''}`} onClick={() => open(trace)}>
              <td className="col-time" title={`${formatFullDateTime(trace.ts)} · ${formatRelative(trace.ts)}`}>{formatListTime(trace.ts)}</td>
              <td className="col-id"><IdCell value={trace.card_id} kind="card" onOpen={trace.card_id ? () => setQuery(String(trace.card_id)) : undefined} openTitle="只看这张卡"/></td>
              <td className="col-model" title={trace.exposed_model}><span className="clip clip-model">{trace.exposed_model ?? '—'}</span></td>
              <td className="col-result"><span className="result-cell">
                <StatusBadge view={view}/>
                {trace.error_class && <span className="result-reason" title={trace.error_class}>{errorClassLabel(trace.error_class)}</span>}
                {attempts > 1 && <span className="retry-count" title={`共尝试 ${attempts} 次`}>↻{attempts - 1}</span>}
              </span></td>
              <td className={`num ${ttftTone(trace.ttft_ms)}`} title={trace.ttft_ms == null ? '此请求没有计时（旧记录或非流式输出）' : undefined}>{formatDuration(trace.ttft_ms)}</td>
              <td className={`num col-speed${typeof trace.tokens_per_second === 'number' && trace.tokens_per_second < 10 ? ' is-warning' : ''}`}>{formatSpeed(trace.tokens_per_second)}</td>
              <td className="num col-tokens" title={`输入 ${formatCount(trace.input_tokens ?? 0)} · 输出 ${formatCount(trace.output_tokens ?? 0)}`}>{formatTokenCount(trace.input_tokens ?? 0)} / {formatTokenCount(trace.output_tokens ?? 0)}</td>
              <td className="num">{attempts ? formatDuration(latency(trace)) : '—'}</td>
              <td className="num">{formatCharge(trace.credits_charged)}</td>
              <td className="col-actions"><span className="row-actions">
                {recent && trace.invocation_id && <span className="doc-hint" title="24 小时内可查看请求内容"><IconDoc/></span>}
                <button type="button" className="btn-text" onClick={event => {event.stopPropagation(); open(trace);}}>详情</button>
              </span></td>
            </tr>;
          })}
          {!filtered.length && <TableState colSpan={10} loading={loading || (!!exactCard && !cardScope?.traces && !cardScope?.failed)} failed={failed && !traces.length} onRetry={() => void refresh()}
            empty={source.length ? '没有匹配的请求' : '暂无请求记录'}
            action={source.length && filtersActive ? <button type="button" className="btn btn-small" onClick={resetFilters}>清除筛选</button> : undefined}/>}
        </tbody>
      </table></div>
      <Pager page={currentPage} pageSize={PAGE_SIZE} total={filtered.length} onPage={setPage}/>
    </div>

    {selected && <TraceDrawer trace={selected} hasPrev={selectedIndex > 0} hasNext={selectedIndex >= 0 && selectedIndex < filtered.length - 1}
      onMove={move} onClose={() => setSelectedId(null)} onFilterCard={cardId => {setQuery(cardId); setStatus('ALL');}}
      cardKnown={cards.some(card => card.id === selected.card_id)} onOpenCard={onOpenCard}/>}
  </div>;
}

function TraceDrawer({trace, hasPrev, hasNext, onMove, onClose, onFilterCard, cardKnown, onOpenCard}: {
  trace: AdminTrace;
  hasPrev: boolean;
  hasNext: boolean;
  onMove: (step: number) => void;
  onClose: () => void;
  onFilterCard: (cardId: string) => void;
  cardKnown: boolean;
  onOpenCard: (cardId: string) => void;
}) {
  const [timing, setTiming] = useState<{id: string; reply: TraceReply | null} | null>(null);
  const reply = timing?.id === trace.id ? timing.reply : null;
  const view = traceStatusView(trace.status);
  const chain = trace.attempt_chain ?? [];
  const ttft = typeof trace.ttft_ms === 'number' ? trace.ttft_ms : reply?.ttftMs ?? null;
  const speed = typeof trace.tokens_per_second === 'number' ? trace.tokens_per_second : reply?.tokensPerSecond ?? null;
  const lastError = [...chain].reverse().find(attempt => attempt.error)?.error;
  return <Drawer id="trace-detail" label="请求详情" onClose={onClose}>
    <header className="drawer-head">
      <div className="drawer-title">
        <StatusBadge view={view}/>
        <span className="drawer-model">{trace.exposed_model ?? '—'}</span>
        <span className="muted" title={formatFullDateTime(trace.ts)}>{formatDateTime(trace.ts)}（{formatRelative(trace.ts)}）</span>
      </div>
      <div className="drawer-tools">
        <button type="button" className="btn-icon" aria-label="上一条" title="上一条（↑）" disabled={!hasPrev} onClick={() => onMove(-1)}><IconChevronUp/></button>
        <button type="button" className="btn-icon" aria-label="下一条" title="下一条（↓）" disabled={!hasNext} onClick={() => onMove(1)}><IconChevronDown/></button>
        <button type="button" className="btn-icon" aria-label="复制请求 ID" title="复制请求 ID" onClick={() => void copyText(trace.id, '已复制请求 ID')}><IconCopy/></button>
        <button type="button" className="btn-icon" aria-label="关闭详情" title="关闭（Esc）" onClick={onClose}><IconClose/></button>
      </div>
    </header>
    <div className="drawer-body">
      <dl className="metric-strip">
        <div><dt>首字</dt><dd className={ttftTone(ttft)}>{formatDuration(ttft)}</dd></div>
        <div><dt>速度</dt><dd>{formatSpeed(speed)}</dd></div>
        <div><dt>耗时</dt><dd>{chain.length ? formatDuration(latency(trace)) : '—'}</dd></div>
        <div><dt>Tokens</dt><dd title={reply ? `缓存读 ${formatCount(reply.cacheReadTokens)} · 缓存写 ${formatCount(reply.cacheWriteTokens)}` : `输入 ${formatCount(trace.input_tokens ?? 0)} · 输出 ${formatCount(trace.output_tokens ?? 0)}`}>
          {formatTokenCount(trace.input_tokens ?? 0)} / {formatTokenCount(trace.output_tokens ?? 0)}</dd></div>
        <div><dt>扣费</dt><dd>{formatCharge(trace.credits_charged)}</dd></div>
        <div><dt>成本</dt><dd>{trace.provider_cost_micro_cny ? formatMoney(trace.provider_cost_micro_cny) : '—'}</dd></div>
      </dl>
      <dl className="detail-list">
        <dt>卡密</dt><dd>
          <IdCell value={trace.card_id} kind="card"/>
          {trace.card_id && <button type="button" className="btn-text" onClick={() => onFilterCard(String(trace.card_id))}>只看这张卡</button>}
          {trace.card_id && cardKnown && <button type="button" className="btn-text" onClick={() => onOpenCard(String(trace.card_id))}>去卡密资产</button>}
        </dd>
        <dt>供应商</dt><dd>{trace.provider_id ?? '—'}</dd>
        <dt>请求 ID</dt><dd><IdCell value={trace.id} kind="trace"/></dd>
        {trace.error_class && <><dt>失败原因</dt><dd><b>{errorClassLabel(trace.error_class)}</b> <span className="mono muted">{trace.error_class}</span></dd></>}
        {lastError && <><dt>上游错误</dt><dd className="error-line"><span>{lastError}</span></dd></>}
      </dl>
      <section className="attempts">
        <h4>尝试链{chain.length > 1 ? ` · ${chain.length} 次` : ''}</h4>
        {chain.length ? <ol className="attempt-list">{chain.map((attempt, index) => <li key={index} className={attempt.success ? 'is-ok' : 'is-failed'}>
          <span className="attempt-index">{index + 1}</span>
          <div>
            <p><span className="mono">{attempt.provider_id ?? '—'} / {attempt.key_id ?? '—'}</span> · {attempt.success ? '成功' : '失败'} · {formatDuration(attempt.latency_ms)}</p>
            {attempt.error && <p className="attempt-error">{attempt.error}</p>}
          </div>
        </li>)}</ol> : <p className="muted">没有记录尝试</p>}
      </section>
      <ContentSection key={trace.id} trace={trace} onReply={value => setTiming({id: trace.id, reply: value})}/>
    </div>
  </Drawer>;
}

type ContentState = {status: 'idle' | 'loading'} | {status: 'loaded'; data: TraceContent} | {status: 'missing' | 'error'; message: string};
type ContentTab = 'conversation' | 'reply' | 'raw';

/** Loads the request content only when a tab is opened; every read is logged by the server. */
function ContentSection({trace, onReply}: {trace: AdminTrace; onReply: (reply: TraceReply | null) => void}) {
  const [tab, setTab] = useState<ContentTab>('conversation');
  const [content, setContent] = useState<ContentState>({status: 'idle'});
  const alive = useRef(true);
  useEffect(() => {alive.current = true; return () => {alive.current = false;};}, []);
  const nowSecs = Date.now() / 1000;
  const expiresAt = content.status === 'loaded' ? content.data.expiresAt : Number(trace.ts) + RETENTION;
  // A little grace for a clock that runs ahead of the server's.
  const expired = content.status !== 'loaded' && nowSecs - Number(trace.ts) > RETENTION + 600;

  const load = async () => {
    if (!trace.invocation_id) {setContent({status: 'missing', message: '这次请求没有内容编号'}); return;}
    setContent({status: 'loading'});
    try {
      const data = await adminApi.getTraceContent(trace.invocation_id);
      if (!alive.current) return;
      if (data.success !== true) throw new Error('服务器未确认读取成功');
      setContent({status: 'loaded', data});
      onReply(data.reply ?? null);
    } catch (error) {
      if (!alive.current) return;
      setContent(error instanceof AdminApiError && error.status === 404 ? {status: 'missing', message: error.message} : {status: 'error', message: errorText(error)});
    }
  };

  const until = new Date(expiresAt * 1000);
  const untilText = until.toDateString() === new Date().toDateString() ? formatClock(expiresAt) : `${formatShortDate(expiresAt)} ${formatClock(expiresAt)}`;
  const hoursLeft = Math.max(0, Math.ceil((expiresAt - nowSecs) / 3600));

  const labels: Record<ContentTab, string> = {conversation: '对话', reply: '模型回复', raw: '原始 JSON'};
  return <section className="content-section">
    <div className="content-head">
      <h4>内容</h4>
      {expired ? <span className="muted">内容只保留 24 小时，已过期</span>
        : content.status === 'missing' ? null
        : <span className="muted">保留至 {untilText}（剩 {hoursLeft} 小时）</span>}
    </div>
    {!expired && content.status === 'idle' && <p><button type="button" className="btn btn-small" onClick={() => void load()}>查看内容（会记录）</button></p>}
    {!expired && content.status === 'loading' && <div className="skeleton" role="status" aria-label="正在加载"><span className="skeleton-bar"/><span className="skeleton-bar"/><span className="skeleton-bar"/></div>}
    {content.status === 'missing' && <p className="empty-note">{content.message}</p>}
    {content.status === 'error' && <div className="list-state" role="status"><p>读取失败：{content.message}</p><button type="button" className="btn btn-small" onClick={() => void load()}>重试</button></div>}
    {content.status === 'loaded' && <>
      <div className="content-tabs" role="tablist" aria-label="内容">
        {(Object.keys(labels) as ContentTab[]).map(value =>
          <button key={value} type="button" role="tab" aria-selected={tab === value} className={`content-tab${tab === value ? ' is-active' : ''}`} onClick={() => setTab(value)}>{labels[value]}</button>)}
      </div>
      <div className="drawer-content" role="tabpanel" tabIndex={0} aria-label={labels[tab]}>
        {tab === 'conversation' && <ConversationView content={content.data}/>}
        {tab === 'reply' && <ReplyView reply={content.data.reply}/>}
        {tab === 'raw' && <RawView content={content.data}/>}
      </div>
    </>}
  </section>;
}
