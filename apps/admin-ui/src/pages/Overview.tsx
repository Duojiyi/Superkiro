// 运营概览: real totals for the last 24 hours or 7 days, the requests hour by hour, what
// needs someone's attention, and each provider's health.
import {useState, type ReactNode} from 'react';
import {loadAdjustment} from '../adjustment';
import {CARD_CHANGE_KEY} from '../cardSupport';
import {attemptsFromTraces, failingModels, failureText, failureTone, kindsText, type AttemptWindow} from '../health';
import type {AdminActivityWindow, AdminTrace} from '../api';
import {EstimateTag, FilterTabs, StatusBadge, TableState, TopbarActions} from '../components/ui';
import {IconCheck, IconWarning} from '../components/icons';
import {formatBytes, formatCount, formatCredits, formatCreditsMicro, formatDuration, formatFullDateTime, formatMoney, formatMoneyExact, formatPercent, formatRemaining, shortId} from '../format';
import {brokenRoutes, modelName, nameList, targetProblem} from '../routes';
import {cardState, cooldownText, failureLabel, keyAlert, keyCooldownLeft, keyStatusView, storageLevel, TRACE_IN_PROGRESS} from '../status';
import type {Intent, Row, Tab} from '../types';
import type {Failures, WorkspaceData} from '../Workspace';

type Range = '24h' | '7d';
const DAY = 86400;
/** What needs attention, most urgent first; `links` when it concerns more than one place. */
interface AttentionItem {text: string; tone: 'warning' | 'danger' | 'info'; go: () => void; links?: Array<{label: string; go: () => void}>}
const URGENCY = {danger: 0, warning: 1, info: 2};
/** A success rate's colour, as the 成功率 KPI has it. */
const rateTone = (rate: number | null) => rate === null ? undefined : rate < 90 ? 'danger' as const : rate < 95 ? 'warning' as const : undefined;

/** Totals from the loaded traces, for servers that do not report activity yet. */
function windowFromTraces(traces: AdminTrace[], start: number): AdminActivityWindow {
  const window: AdminActivityWindow = {requests: 0, succeeded: 0, failed: 0, clientAborted: 0, creditsCharged: 0, inputTokens: 0, outputTokens: 0, providerCostMicroCny: 0, activeCards: 0};
  const cards = new Set<string>();
  const timed: number[] = [];
  for (const trace of traces) {
    if (Number(trace.ts) <= start || TRACE_IN_PROGRESS.includes(String(trace.status))) continue;
    window.requests++;
    if (trace.status === 'success') window.succeeded++;
    else if (trace.status === 'error') window.failed++;
    else if (trace.status === 'client_aborted') window.clientAborted++;
    window.creditsCharged += Number(trace.credits_charged ?? 0);
    if (Number(trace.credits_charged ?? 0) > 0 && trace.card_id) cards.add(trace.card_id);
    if (typeof trace.ttft_ms === 'number') timed.push(trace.ttft_ms);
  }
  window.activeCards = cards.size;
  timed.sort((a, b) => a - b);
  window.timedRequests = timed.length;
  window.ttftMedianMs = timed.length ? timed[Math.floor((timed.length - 1) / 2)] : null;
  window.ttftP90Ms = timed.length ? timed[Math.ceil(timed.length * 0.9) - 1] : null;
  return window;
}

function Kpi({label, value, sub, tone, estimate}: {label: string; value: string; sub?: ReactNode; tone?: 'warning' | 'danger'; estimate?: string}) {
  return <section className="panel kpi">
    <p className="kpi-label">{label}{estimate && <EstimateTag title={estimate}/>}</p>
    <strong className={`kpi-value${tone ? ` is-${tone}` : ''}`}>{value}</strong>
    {sub && <div className="kpi-sub">{sub}</div>}
  </section>;
}

export default function OverviewPage({data, loading, failures, providersLoaded, operator, onNavigate, onRetry}: {
  data: WorkspaceData;
  loading: boolean;
  failures: Failures;
  providersLoaded: boolean;
  operator: string | null;
  onNavigate: (tab: Tab, intent?: Intent) => void;
  onRetry: () => void;
}) {
  const [range, setRange] = useState<Range>('24h');
  const now = Date.now(), nowSecs = now / 1000;
  const activity = data.stats?.activity;
  const start = nowSecs - (range === '24h' ? DAY : 7 * DAY);
  const period = activity ? (range === '24h' ? activity.last24h : activity.last7d) : data.traces.length ? windowFromTraces(data.traces, start) : null;
  const cover = activity?.tracesCoverFromSecs;
  const oldestTrace = data.traces.length ? Math.min(...data.traces.map(trace => Number(trace.ts))) : null;
  // Request counts come from the kept traces: when they begin after the window starts, the
  // counts are partial and say so. Credits come from the ledger and are complete.
  const requestEstimate = activity
    ? (typeof cover === 'number' && cover > start ? `调用记录从 ${formatFullDateTime(cover)} 起，更早的请求没有计入` : undefined)
    : period ? `按最近 ${data.traces.length} 条调用记录统计${oldestTrace ? `（${formatFullDateTime(oldestTrace)} 起）` : ''}` : undefined;
  const rate = period && period.requests ? period.succeeded / period.requests * 100 : null;
  const faceValue = data.settings?.credit_face_value_cny ?? data.financials?.settings?.credit_face_value_cny;

  // Cards by their status as it works now: one past its date is expired, whatever the server records.
  const currentCards = data.cards.filter(card => card.archivedAt == null && cardState(card, nowSecs) !== 'voided');
  const activeCards = currentCards.filter(card => cardState(card, nowSecs) === 'active');
  const unactivated = currentCards.filter(card => cardState(card, nowSecs) === 'unactivated').length;
  const usableBalance = currentCards.filter(card => ['active', 'unactivated', 'frozen'].includes(cardState(card, nowSecs))).reduce((sum, card) => sum + card.pointsAvailable, 0);

  // Hour by hour over the last day; the rightmost bar is the current hour.
  const hours = activity?.hourly?.length ? activity.hourly : Array.from({length: 24}, (_, index) => {
    const startSecs = (Math.floor(nowSecs / 3600) - 23 + index) * 3600;
    const inHour = data.traces.filter(trace => Number(trace.ts) >= startSecs && Number(trace.ts) < startSecs + 3600 && !TRACE_IN_PROGRESS.includes(String(trace.status)));
    return {startSecs, requests: inHour.length, failed: inHour.filter(trace => trace.status === 'error').length};
  });
  const peak = Math.max(1, ...hours.map(hour => hour.requests));
  const hourLabel = (secs: number) => String(new Date(secs * 1000).getHours()).padStart(2, '0');
  const chartEstimate = activity
    ? (typeof cover === 'number' && cover > nowSecs - DAY ? `调用记录从 ${formatFullDateTime(cover)} 起` : undefined)
    : `按最近 ${data.traces.length} 条调用记录统计`;

  // Only what someone can act on, each naming what it is about and landing on it (the Key or
  // provider marked, or the filtered list). Nothing at zero.
  const attention: AttentionItem[] = [];
  const providerName = (id: unknown) => String(data.providers.find(provider => provider.id === id)?.name ?? id);
  const toKey = (key: Row) => () => onNavigate('providers', {providers: {key: String(key.id)}});
  const toProvider = (id: unknown) => () => onNavigate('providers', {providers: {provider: String(id)}});
  // The saved state nears the size at which every request is refused: archive the ledger.
  const storage = storageLevel(data.stats);
  if (storage && storage.level !== 'ok') attention.push({text: `账本存储 ${formatBytes(storage.bytes)} / ${formatBytes(storage.ceiling)}，${storage.level === 'now'
    ? '请现在归档：到上限时所有请求都会被拒绝' : '建议归档旧账本'}`, tone: storage.level === 'now' ? 'danger' : 'warning', go: () => onNavigate('security')});
  // Shown models nothing can serve, each with why and links to what to fix.
  const broken = providersLoaded ? brokenRoutes(data.models, {providers: data.providers, keys: data.providerKeys, nowSecs}) : [];
  for (const {model, route} of broken.filter(entry => entry.route.down)) {
    const problem = targetProblem(route.primary, data.providers, nowSecs), fix = route.primary.keys?.[0];
    const links = [...(fix ? [{label: '查看 Key', go: toKey(fix)}] : ['no_key', 'provider_disabled'].includes(String(route.primary.problem)) ? [{label: '查看供应商', go: toProvider(route.primary.provider_id)}] : []),
      {label: '去模型与定价', go: () => onNavigate('models')}];
    attention.push({text: `${modelName(model, data.models, data.groups)}：${route.backups.length ? `主线路 ${problem}，${route.backups.length} 条备用线路也不能用` : `唯一线路 ${problem}`}`,
      tone: 'danger', go: links[0].go, links});
  }
  // Keys by state: each named when there are a few, else the first few and how many.
  const keyItems = (keys: Row[], one: (key: Row) => string, many: string, tone: AttentionItem['tone']) => {
    if (keys.length <= 3) for (const key of keys) attention.push({text: one(key), tone, go: toKey(key)});
    else attention.push({text: `${keys.length} 个 Key ${many}：${nameList(keys.map(key => String(key.id)), 3)}`, tone, go: toKey(keys[0])});
  };
  // A Key of a disabled provider serves nothing, so its cooldown needs no attention.
  const liveKeys = data.providerKeys.filter(key => key.enabled !== false && data.providers.find(provider => provider.id === key.provider_id)?.enabled !== false);
  const coolingKeys = liveKeys.filter(key => keyAlert(key, nowSecs) === 'cooldown');
  const left = coolingKeys.map(key => keyCooldownLeft(key, nowSecs)).filter(value => value > 0);
  keyItems(coolingKeys, key => `Key ${String(key.id)}（${providerName(key.provider_id)}）冷却中${keyCooldownLeft(key, nowSecs) > 0 ? `，${cooldownText(keyCooldownLeft(key, nowSecs))}后恢复` : ''}`,
    `冷却中${left.length ? `（最早 ${cooldownText(Math.min(...left))}后恢复）` : ''}`, 'warning');
  keyItems(liveKeys.filter(key => keyAlert(key, nowSecs) === 'degraded'), key => `Key ${String(key.id)}（${providerName(key.provider_id)}）冷却后试用中`, '冷却后试用中', 'warning');
  keyItems(liveKeys.filter(key => keyAlert(key, nowSecs) === 'unhealthy'),
    key => `Key ${String(key.id)}（${providerName(key.provider_id)}）不可用${failureLabel(key.last_error) ? `：${failureLabel(key.last_error)}` : ''}`, '不可用', 'danger');
  // Models that fail, from the server's count of requests by model: each named, with a link to its
  // failed requests. Without it (older servers), failed requests of the last hour from the traces.
  const failing = activity?.modelHealth ? failingModels(activity.modelHealth) : null;
  if (failing) {
    const each = failing.length <= 3 ? failing : [];
    for (const entry of each) attention.push({text: entry.text, tone: 'danger', go: () => onNavigate('traces', {traces: {status: 'error', window: entry.window, model: entry.model}})});
    if (failing.length > 3) attention.push({text: `${failing.length} 个模型有失败：${nameList(failing.map(entry => entry.model), 3)}`, tone: 'danger',
      go: () => onNavigate('traces', {traces: {status: 'error', window: failing.some(entry => entry.window === 'day') ? 'day' : 'hour'}})});
  }
  // Failed requests, by model: one model is named and filtered on, several give the busiest.
  const failedLastHour = data.traces.filter(trace => trace.status === 'error' && Number(trace.ts) > nowSecs - 3600);
  if (!failing && failedLastHour.length) {
    const byModel = [...failedLastHour.reduce((counts, trace) => counts.set(String(trace.exposed_model ?? '—'), (counts.get(String(trace.exposed_model ?? '—')) ?? 0) + 1), new Map<string, number>())].sort((a, b) => b[1] - a[1]);
    const only = byModel.length === 1 && byModel[0][0] !== '—' ? byModel[0][0] : null;
    attention.push({text: only ? `近 1 小时 ${only} 失败 ${failedLastHour.length} 次` : `近 1 小时 ${failedLastHour.length} 次失败请求（最多：${byModel[0][0]} ${byModel[0][1]} 次）`,
      tone: 'danger', go: () => onNavigate('traces', {traces: {status: 'error', window: 'hour', ...(only ? {model: only} : {})}})});
  }
  const frozen = currentCards.filter(card => cardState(card, nowSecs) === 'frozen').length;
  if (frozen) attention.push({text: `${frozen} 张卡已冻结`, tone: 'warning', go: () => onNavigate('cards', {cards: {status: 'FROZEN'}})});
  const expiring = currentCards.filter(card => ['active', 'frozen'].includes(cardState(card, nowSecs)) && card.validUntil != null && card.validUntil <= nowSecs + 7 * DAY).length;
  if (expiring) attention.push({text: `${expiring} 张卡 7 天内到期`, tone: 'info', go: () => onNavigate('cards', {cards: {quick: 'expiring'}})});
  const low = activeCards.filter(card => card.pointsTotal > 0 && card.pointsAvailable / card.pointsTotal < 0.1).length;
  if (low) attention.push({text: `${low} 张卡余额低于 10%`, tone: 'info', go: () => onNavigate('cards', {cards: {quick: 'low'}})});
  try {
    const pending = operator ? loadAdjustment(sessionStorage, operator) : null;
    if (pending) attention.push({text: `卡 ${shortId(pending.cardId, 'card')} 有一笔调账结果未确认`, tone: 'danger', go: () => onNavigate('cards', {cards: {status: 'ALL', search: pending.cardId, open: pending.cardId}})});
  } catch {
    attention.push({text: '调账恢复记录无法读取', tone: 'danger', go: () => onNavigate('cards')});
  }
  const stored = (key: string) => {try {return !!sessionStorage.getItem(key);} catch {return false;}};
  if (stored('admin-pending-issuance:v1')) attention.push({text: '上次批量生成的结果未确认', tone: 'danger', go: () => onNavigate('cards')});
  if (stored(CARD_CHANGE_KEY)) attention.push({text: '上次的卡密修改结果未确认', tone: 'danger', go: () => onNavigate('cards')});
  if (stored('admin-pending-announcement:v1')) attention.push({text: '上次公告发布的结果未确认', tone: 'danger', go: () => onNavigate('announcements')});
  for (const notice of data.announcements) {
    if (!notice.enabled || !notice.expires_at || notice.expires_at <= nowSecs || notice.expires_at > nowSecs + DAY) continue;
    attention.push({text: `公告「${notice.title}」${formatRemaining(notice.expires_at, now).text.replace('剩 ', '')}后到期`, tone: 'info', go: () => onNavigate('announcements')});
  }
  // Today's money: face-value revenue and configured cost of every settled request, the margin over
  // the ones whose cost is known; a model sold below its cost today is raised.
  const today = data.financialsToday;
  const todayRevenue = today?.estimates?.usageFaceValueMicroCny ?? today?.dashboard?.revenue_micro_cny;
  const todayCost = today?.estimates?.configuredProviderCostMicroCny ?? today?.dashboard?.provider_cost_micro_cny;
  const todayMargin = today?.margin;
  const todayFace = today?.settings?.credit_face_value_cny;
  const losing = (today?.modelRankings ?? []).map(row => {
    const credits = Number(row.credits_charged ?? 0), cost = Number(row.provider_cost_micro_cny ?? 0), value = typeof todayFace === 'number' ? credits * todayFace : 0;
    const rate = !(Number(row.requests ?? 0) > 0) || !cost ? null : typeof row.margin_percentage === 'number' ? row.margin_percentage : value ? (value - cost) / value * 100 : null;
    return {model: String(row.model_id ?? '—'), rate};
  }).filter((row): row is {model: string; rate: number} => row.rate !== null && row.rate < 0).sort((a, b) => a.rate - b.rate);
  if (losing.length) attention.push({text: losing.length <= 2
    ? `今天 ${losing.map(row => `${row.model} 毛利 ${formatPercent(row.rate)}`).join('、')}：售价低于采购价，按成本在亏`
    : `今天 ${losing.length} 个模型按成本在亏：${nameList(losing.map(row => row.model), 3)}`, tone: 'danger', go: () => onNavigate('reconciliation')});
  attention.sort((a, b) => URGENCY[a.tone] - URGENCY[b.tone]);
  const unknown = failures.cards || failures.providers || failures.traces;
  // Shown models whose primary route cannot serve: those nothing can serve, and those a backup serves.
  const describe = (entries: typeof broken) => nameList(entries.map(({model, route}) => `${modelName(model, data.models, data.groups)}（${targetProblem(route.primary, data.providers, nowSecs)}）`), 4);
  const down = broken.filter(entry => entry.route.down), takeover = broken.filter(entry => !entry.route.down);

  // Every upstream attempt over the last 24 hours by provider (a primary's failures show, though its
  // backup answered): from the server, else counted from the traces the console has.
  const attempts24: Map<string, AttemptWindow> = activity?.providerAttempts
    ? new Map(activity.providerAttempts.map(entry => [entry.providerId, entry.last24h]))
    : attemptsFromTraces(data.traces, nowSecs).providers;
  const attemptsEstimate = activity?.providerAttempts ? undefined : `按最近 ${data.traces.length} 条调用记录的尝试统计`;
  const providerRows = data.providers.map(provider => {
    const keys = data.providerKeys.filter(key => key.provider_id === provider.id);
    const counts = new Map<string, {count: number; tone: string}>();
    for (const key of keys) {
      const view = keyStatusView(key, nowSecs);
      const label = view.label.split(' · ')[0];
      counts.set(label, {count: (counts.get(label)?.count ?? 0) + 1, tone: view.tone});
    }
    const reported = activity?.providers?.find(entry => entry.providerId === provider.id);
    const traced = data.traces.filter(trace => trace.provider_id === provider.id && Number(trace.ts) > nowSecs - DAY && !TRACE_IN_PROGRESS.includes(String(trace.status)));
    const requests = reported ? reported.requests : traced.length;
    const failed = reported ? reported.failed : traced.filter(trace => trace.status === 'error').length;
    const timed = traced.map(trace => trace.ttft_ms).filter((value): value is number => typeof value === 'number').sort((a, b) => a - b);
    const median = reported ? reported.ttftMedianMs : timed.length ? timed[Math.floor((timed.length - 1) / 2)] : null;
    return {provider, keys, counts, rate: requests ? (requests - failed) / requests * 100 : null, median, attempts: attempts24.get(String(provider.id))};
  });

  return <div className="page-stack">
    <TopbarActions><FilterTabs label="统计范围" value={range} onChange={setRange} options={[{value: '24h', label: '近 24 小时'}, {value: '7d', label: '近 7 天'}]}/></TopbarActions>
    {down.length > 0 && <div role="alert" className="banner banner-danger"><IconWarning/>
      <span className="banner-text"><b>{down.length} 个在售模型无可用线路</b>，客户请求会失败：{describe(down)}</span>
      <button type="button" className="btn btn-small" onClick={() => onNavigate('models')}>去模型与定价</button></div>}
    {takeover.length > 0 && <div role="status" className="banner banner-warning"><IconWarning/>
      <span className="banner-text"><b>{takeover.length} 个在售模型的主线路不可用</b>，正由备用线路服务：{describe(takeover)}</span>
      <button type="button" className="btn btn-small" onClick={() => onNavigate('models')}>去模型与定价</button></div>}
    <div className="kpi-row">
      <Kpi label="请求" estimate={requestEstimate} value={period ? formatCount(period.requests) : '—'}
        sub={period && <>
          <button type="button" className="link" onClick={() => onNavigate('traces', {traces: {status: 'error', window: range === '24h' ? 'day' : 'all'}})}>失败 {formatCount(period.failed)}</button>
          {period.clientAborted > 0 && <span> · 中断 {formatCount(period.clientAborted)}</span>}
        </>}/>
      <Kpi label="成功率" estimate={requestEstimate} value={formatPercent(rate)} tone={rateTone(rate)}
        sub={period ? `${formatCount(period.succeeded)} 次成功` : undefined}/>
      <Kpi label="首字耗时" value={period?.timedRequests ? `${formatDuration(period.ttftMedianMs)} / ${formatDuration(period.ttftP90Ms)}` : '—'}
        sub={period?.timedRequests ? `中位数 / P90 · ${formatCount(period.timedRequests)} 次有计时` : period ? '没有计时的请求' : undefined}/>
      <Kpi label="消耗积分" value={period ? formatCreditsMicro(period.creditsCharged) : '—'}
        sub={period && typeof faceValue === 'number' ? <span title="按积分面值折算">≈ {formatMoney(period.creditsCharged * faceValue)}</span> : undefined}/>
      <Kpi label="在用卡密" value={failures.cards && !data.cards.length ? '—' : `${formatCount(activeCards.length)} 张`}
        sub={data.cards.length ? `未激活 ${formatCount(unactivated)} · 可用 ${formatCredits(usableBalance)} 积分` : undefined}/>
    </div>

    <div className="overview-grid">
      <section className="panel chart-panel">
        <div className="panel-head"><h3>请求量 · 近 24 小时</h3>{chartEstimate && <EstimateTag title={chartEstimate}/>}</div>
        {!activity && !data.traces.length ? <TableStateBlock loading={loading} failed={!!failures.stats && !!failures.traces} onRetry={onRetry}/> :
          <div className="request-chart" role="img" aria-label={`近 24 小时 ${hours.reduce((sum, hour) => sum + hour.requests, 0)} 次请求，失败 ${hours.reduce((sum, hour) => sum + hour.failed, 0)} 次`}>
            {hours.map((hour, index) => {
              const current = index === hours.length - 1;
              const end = new Date((hour.startSecs + 3600) * 1000);
              const title = `${formatFullDateTime(hour.startSecs).slice(5, 16)}–${String(end.getHours()).padStart(2, '0')}:00 · ${hour.requests} 次 · 失败 ${hour.failed}`;
              return <div key={hour.startSecs} className={`chart-column${current ? ' is-current' : ''}`} data-requests={hour.requests} data-failed={hour.failed} title={title}>
                <div className="chart-track">
                  <div className="chart-bar" style={{height: `${hour.requests / peak * 100}%`}}>
                    {hour.failed > 0 && <div className="chart-bar-failed" style={{height: `${hour.failed / Math.max(1, hour.requests) * 100}%`}}/>}
                  </div>
                </div>
                <small className="chart-label">{index % 3 === 0 || current ? hourLabel(hour.startSecs) : ''}</small>
              </div>;
            })}
          </div>}
        <div className="chart-legend"><span className="legend-ok">成功</span><span className="legend-failed">失败</span></div>
        {/* Today from local midnight: what the requests settled so far brought in and cost. */}
        <div className="today-money" role="group" aria-label="今日收入">
          <div><span className="today-label">今日收入<EstimateTag title="今天 0 点起结算的请求，按积分面值折算；不含实际收款"/></span>
            <b title={typeof todayRevenue === 'number' ? formatMoneyExact(todayRevenue) : '今天的财务没有读取成功'}>{typeof todayRevenue === 'number' ? formatMoney(todayRevenue) : '—'}</b></div>
          <div><span className="today-label">成本</span><b title={typeof todayCost === 'number' ? formatMoneyExact(todayCost) : undefined}>{typeof todayCost === 'number' ? formatMoney(todayCost) : '—'}</b></div>
          <div><span className="today-label">毛利</span>
            <b className={todayMargin && todayMargin.grossProfitMicroCny < 0 ? 'is-danger' : undefined} title={todayMargin ? formatMoneyExact(todayMargin.grossProfitMicroCny) : undefined}>{todayMargin ? formatMoney(todayMargin.grossProfitMicroCny) : '—'}</b>
            {todayMargin && <small className={todayMargin.uncostedRequests ? 'is-warning' : 'muted'}>{typeof todayMargin.marginPercentage === 'number' ? formatPercent(todayMargin.marginPercentage) : ''}{todayMargin.uncostedRequests ? `${typeof todayMargin.marginPercentage === 'number' ? ' · ' : ''}另有 ${formatCount(todayMargin.uncostedRequests)} 次未设成本` : ''}</small>}</div>
          <button type="button" className="btn-text" onClick={() => onNavigate('reconciliation')}>财务对账 ›</button>
        </div>
      </section>

      <section className="panel attention">
        <h3>需要关注</h3>
        {attention.length ? <ul className="attention-list">
          {attention.map(item => <li key={item.text}>{item.links
            ? <div className={`attention-item is-${item.tone}`}><span className="attention-text">{item.text}</span>
              <span className="attention-links">{item.links.map(link => <button key={link.label} type="button" className="btn-text btn-small" onClick={link.go}>{link.label}</button>)}</span></div>
            : <button type="button" className={`attention-item is-${item.tone}`} onClick={item.go}>{item.text}<span aria-hidden="true">›</span></button>}</li>)}
        </ul> : loading && !providersLoaded ? <p className="muted">正在加载…</p>
          : unknown ? <p className="muted">部分数据没有加载，暂时无法确认</p>
          : <p className="all-good"><IconCheck/>一切正常</p>}
      </section>
    </div>

    <section className="panel" aria-label="服务健康">
      <div className="panel-head"><h3>服务健康 <span className="panel-period">近 24 小时</span></h3>{attemptsEstimate && <EstimateTag title={attemptsEstimate}/>}</div>
      <div className="table-scroll"><table className="table health-table">
        <thead><tr><th>供应商</th><th className="col-status">状态</th><th>Key</th>
          <th className="num" title="近 24 小时发给这个供应商的每一次尝试，包括失败后改由备用线路完成的">尝试</th><th className="num">失败</th>
          <th className="num" title="失败后由别的供应商接着完成的次数">被备用接管</th><th>失败原因</th><th className="num">首字中位数</th><th className="col-actions"><span className="sr-only">操作</span></th></tr></thead>
        <tbody>
          {providerRows.map(({provider, keys, counts, rate, median, attempts}) => {
            const tone = attempts ? failureTone(attempts.failures, attempts.attempts) : undefined;
            return <tr key={String(provider.id)}>
              <td className="cell-strong">{String(provider.name || provider.id)}</td>
              <td className="col-status"><StatusBadge view={provider.enabled === false ? {label: '已停用', tone: 'neutral'} : {label: '启用', tone: 'success'}}/></td>
              <td>{keys.length ? <span className="key-summary">{[...counts.entries()].map(([label, value]) => <span key={label} className={`dot-label dot-${value.tone}`}>{value.count} {label}</span>)}</span> : <span className="muted">没有 Key</span>}</td>
              <td className="num" title={rate === null ? undefined : `它最终完成的请求成功率 ${formatPercent(rate)}`}>{attempts?.attempts ? formatCount(attempts.attempts) : '—'}</td>
              <td className={`num${tone ? ` is-${tone}` : ''}`} title={attempts?.attempts ? `${formatCount(attempts.failures)} / ${formatCount(attempts.attempts)} 次尝试失败` : undefined}>{attempts?.attempts ? failureText(attempts.failures, attempts.attempts) : '—'}</td>
              <td className="num">{attempts?.takenOver ? formatCount(attempts.takenOver) : '—'}</td>
              <td className="col-kinds">{attempts?.failures ? <span className="clip clip-reason" title={kindsText(attempts.failuresByKind, 10)}>{kindsText(attempts.failuresByKind)}</span> : <span className="muted">—</span>}</td>
              <td className="num">{formatDuration(median)}</td>
              <td className="col-actions"><button type="button" className="btn-text" onClick={toProvider(provider.id)}>查看</button></td>
            </tr>;
          })}
          {!data.providers.length && <TableState colSpan={9} loading={loading} failed={failures.providers} empty="还没有供应商" onRetry={onRetry}
            action={<button type="button" className="btn btn-small" onClick={() => onNavigate('providers')}>添加供应商</button>}/>}
        </tbody>
      </table></div>
    </section>
  </div>;
}

function TableStateBlock({loading, failed, onRetry}: {loading: boolean; failed: boolean; onRetry: () => void}) {
  if (loading) return <div className="skeleton" role="status" aria-label="正在加载"><span className="skeleton-bar"/><span className="skeleton-bar"/></div>;
  if (failed) return <div className="list-state" role="status"><p>加载失败</p><button type="button" className="btn btn-small" onClick={onRetry}>重试</button></div>;
  return <div className="list-state"><p>暂无请求</p></div>;
}
