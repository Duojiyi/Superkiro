// 财务对账: what the kept usage ledger adds up to over a period (今天, 昨天, 本月, 上月, 累计 or chosen days):
// by upstream, to check against each one's bill, and by model; the margin over the requests whose cost
// is known; the cards sold; the balances still owed; and the two settings that turn credits and USD
// prices into yuan. Every yuan figure from usage is an estimate from the ledger.
import {useEffect, useRef, useState} from 'react';
import {adminApi, type AdminFinancials} from '../api';
import {Menu} from '../components/menu';
import {toast} from '../components/toast';
import {EstimateTag, FilterTabs, TableState, TopbarActions} from '../components/ui';
import FinancialPanel from '../FinancialPanel';
import {financialEstimates} from '../financial';
import {formatCount, formatCredits, formatCreditsMicro, formatMoney, formatMoneyExact, formatPercent} from '../format';
import {dateInput, ledgerForPeriod, PERIOD_LABEL, periodRange, periodText, type PeriodKind, type PeriodRange} from '../period';
import type {Refresh, ReportError, Row} from '../types';

const ESTIMATE = '基于保留的用量账本估算，不含实际收款与发票';
const PROVIDER_ESTIMATE = '按配置的采购价估算每个上游该收的钱，用来核对它的账单；Tokens 是这里记下的用量';
const PERIODS: PeriodKind[] = ['today', 'yesterday', 'month', 'lastMonth', 'all', 'custom'];
const errorText = (error: unknown) => (error instanceof Error ? error.message : String(error));

/** Saves a file the browser downloads; the link is kept until the browser has read it. */
function save(name: string, blob: Blob) {
  const url = window.URL.createObjectURL(blob);
  const link = document.createElement('a');
  link.href = url;
  link.download = name;
  document.body.appendChild(link);
  link.click();
  link.remove();
  // The browser reads the file after click() returns; revoking at once can cut it.
  setTimeout(() => window.URL.revokeObjectURL(url), 60000);
}

/** A model's margin: the server's (each entry at the face value it was earned at), else face value against cost. */
function modelMargin(row: AdminFinancials['modelRankings'][number], face: number | undefined): number | null {
  const credits = Number(row.credits_charged ?? 0), cost = Number(row.provider_cost_micro_cny ?? 0);
  if (!(Number(row.requests ?? 0) > 0) || !cost) return null;
  if (typeof row.margin_percentage === 'number' && Number.isFinite(row.margin_percentage)) return row.margin_percentage;
  const value = typeof face === 'number' ? credits * face : 0;
  return value ? (value - cost) / value * 100 : null;
}

export default function FinancePage({financials, providers = [], loading, failed, refresh, reportError, onDirtyChange, onBusyChange, refreshEpoch}: {
  /** The whole kept ledger (累计), as the workspace reads it. */
  financials: AdminFinancials | null;
  providers?: Row[];
  loading: boolean;
  failed: boolean;
  refresh: Refresh;
  reportError: ReportError;
  onDirtyChange: (dirty: boolean) => void;
  refreshEpoch?: number;
  onBusyChange: (busy: boolean) => void;
}) {
  const [kind, setKind] = useState<PeriodKind>('all');
  const [custom, setCustom] = useState(() => {const now = new Date(); return {from: dateInput(new Date(now.getFullYear(), now.getMonth(), 1)), to: dateInput(now)};});
  const range = periodRange(kind, new Date(), custom);
  const rangeKey = range ? `${range.fromSecs ?? ''}-${range.toSecs ?? ''}` : 'invalid';
  // 累计 is the workspace's own reading; any other period is read here, again whenever the console refreshes.
  const [period, setPeriod] = useState<{key: string; data: AdminFinancials | null; error?: string}>({key: '', data: null});
  useEffect(() => {
    if (kind === 'all' || !range) return;
    let current = true;
    setPeriod({key: '', data: null});
    adminApi.getFinancials(range).then(data => {
      if (!current) return;
      if (data.success !== true) throw new Error('服务器未确认读取成功');
      setPeriod({key: rangeKey, data});
    }).catch(error => {if (current) setPeriod({key: rangeKey, data: null, error: errorText(error)});});
    return () => {current = false;};
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [kind, rangeKey, refreshEpoch, financials]);
  const data = kind === 'all' ? financials : period.key === rangeKey ? period.data : null;
  const dataLoading = kind === 'all' ? loading : !!range && period.key !== rangeKey;
  const dataFailed = kind === 'all' ? failed : !!range && period.key === rangeKey && !!period.error;

  const exporting = useRef(false);
  /** The ledger as the server exports it; for a period, only its entries, each kept as written. */
  const exportLedger = async (format: 'json' | 'csv', within: PeriodRange | null = {}) => {
    if (exporting.current || !within) return;
    exporting.current = true;
    try {
      toast.info('正在导出…');
      const blob = await adminApi.exportLedger(format);
      const whole = within.fromSecs === undefined && within.toSecs === undefined;
      if (format === 'json' || whole) save(`ledger-export-${new Date().toISOString().slice(0, 10)}.${format}`, blob);
      else {
        const cut = ledgerForPeriod(await blob.text(), within);
        if (!cut) throw new Error('服务器导出的账本没有 ts 列，不能按统计区间截取；请用“导出全部账本 CSV”');
        const days = periodText(within).replace(' 至 ', '_');
        // With a BOM, spreadsheets read the provider names and reasons as UTF-8.
        save(`ledger-${days}.csv`, new Blob(['\uFEFF' + cut.csv], {type: 'text/csv;charset=utf-8'}));
        toast.success(`已开始下载 ${formatCount(cut.entries)} 条记录（${periodText(within)}），请在浏览器下载列表中确认文件完整`);
        return;
      }
      toast.success('已开始下载，请在浏览器下载列表中确认文件完整');
    } catch (error) {
      reportError(`导出失败：${errorText(error)}`);
    } finally {exporting.current = false;}
  };

  const estimates = financialEstimates(data);
  const face = data?.settings?.credit_face_value_cny;
  const dashboard = data?.dashboard;
  const margin = data?.margin;
  const covered = estimates ? estimates.costedRequests : null;
  const total = estimates ? estimates.costedRequests + estimates.uncostedRequests : null;
  const rankings = data?.modelRankings ?? [];
  const providerName = (id: string) => String(providers.find(provider => provider.id === id)?.name || id);
  const byProvider = data?.byProvider ?? [];
  const sum = (key: keyof (typeof byProvider)[number]) => byProvider.reduce((value, row) => value + Number(row[key] ?? 0), 0);
  const sales = data?.sales, liability = data?.liability;
  const planRows = sales?.byPlan.filter(plan => plan.issuedCards || plan.activatedCards) ?? [];

  return <div className="page-stack">
    <TopbarActions>
      <button type="button" className="btn" disabled={!range} title={range ? `导出统计区间（${periodText(range)}）内的账本` : undefined} onClick={() => void exportLedger('csv', range)}>导出 CSV</button>
      <Menu label="更多导出" className="btn btn-icon-only" items={[{label: '导出全部账本 CSV', onSelect: () => void exportLedger('csv')}, {label: '导出 JSON', onSelect: () => void exportLedger('json')}]}/>
    </TopbarActions>

    <div className="period-bar">
      <FilterTabs label="统计区间" value={kind} onChange={setKind} options={PERIODS.map(value => ({value, label: PERIOD_LABEL[value]}))}/>
      {kind === 'custom' && <span className="period-dates">
        <input type="date" aria-label="开始日期" value={custom.from} max={custom.to} onChange={event => setCustom(value => ({...value, from: event.target.value}))}/>
        <span className="muted">至</span>
        <input type="date" aria-label="结束日期" value={custom.to} min={custom.from} onChange={event => setCustom(value => ({...value, to: event.target.value}))}/>
      </span>}
    </div>
    <p className="range-line" role="status">{range ? `统计区间：${periodText(range)}` : '请选择开始和结束日期（结束不早于开始）'}
      {dataFailed && <span className="is-danger"> · 读取失败：{period.error}</span>}</p>

    <div className="kpi-row">
      <section className="panel kpi"><p className="kpi-label">消耗积分</p>
        <strong className="kpi-value">{dashboard ? formatCreditsMicro(dashboard.total_credits_charged) : '—'}</strong>
        <div className="kpi-sub">{dashboard ? `${formatCount(dashboard.total_requests)} 次结算` : ''}</div></section>
      <section className="panel kpi"><p className="kpi-label">面值收入<EstimateTag title={ESTIMATE}/></p>
        <strong className="kpi-value" title={estimates ? formatMoneyExact(estimates.usageFaceValueMicroCny) : undefined}>{estimates ? formatMoney(estimates.usageFaceValueMicroCny) : '—'}</strong>
        <div className="kpi-sub">{typeof face === 'number' ? `积分面值 ${face} 元/积分` : ''}</div></section>
      <section className="panel kpi"><p className="kpi-label">采购成本<EstimateTag title={ESTIMATE}/></p>
        <strong className="kpi-value" title={estimates ? formatMoneyExact(estimates.configuredProviderCostMicroCny) : undefined}>{estimates ? formatMoney(estimates.configuredProviderCostMicroCny) : '—'}</strong>
        <div className="kpi-sub">{estimates?.uncostedRequests ? '部分请求未设成本' : ''}</div></section>
      <section className="panel kpi"><p className="kpi-label">毛利<EstimateTag title={margin ? `${ESTIMATE}；只算已设成本的请求` : ESTIMATE}/></p>
        {margin
          // Over the requests whose cost is known: one model without a cost no longer blanks the whole.
          ? <><strong className={`kpi-value${margin.grossProfitMicroCny < 0 ? ' is-danger' : ''}`} title={formatMoneyExact(margin.grossProfitMicroCny)}>{formatMoney(margin.grossProfitMicroCny)}</strong>
            <div className="kpi-sub">{typeof margin.marginPercentage === 'number' ? <span className={margin.marginPercentage < 0 ? 'is-danger' : undefined}>毛利率 {formatPercent(margin.marginPercentage)}</span> : '没有收入'}
              {margin.uncostedRequests > 0 && <div className="is-warning" title={`这些请求消耗 ${formatCreditsMicro(margin.uncostedCredits)} 积分，线路没有设采购价，没有算进毛利`}>另有 {formatCount(margin.uncostedRequests)} 次未设成本</div>}</div></>
          : estimates && estimates.uncostedRequests > 0
            ? <><strong className="kpi-value">—</strong><div className="kpi-sub is-warning">{formatCount(estimates.uncostedRequests)} 次请求未设成本，毛利暂不计算</div></>
            : <><strong className="kpi-value" title={estimates ? formatMoneyExact(estimates.faceValueLessCostMicroCny) : undefined}>{estimates ? formatMoney(estimates.faceValueLessCostMicroCny) : '—'}</strong>
              <div className="kpi-sub">{estimates && typeof estimates.faceValueMarginPercentage === 'number' ? `毛利率 ${formatPercent(estimates.faceValueMarginPercentage)}` : ''}</div></>}
      </section>
      <section className="panel kpi"><p className="kpi-label">成本覆盖</p>
        <strong className="kpi-value kpi-fraction" title={covered !== null && total !== null ? `${formatCount(covered)} / ${formatCount(total)} 次请求已设成本` : undefined}>
          {covered !== null && total !== null ? <>{formatCount(covered)}<span className="kpi-of">/ {formatCount(total)}</span></> : '—'}</strong>
        <div className="kpi-sub">{estimates?.uncostedRequests ? `${formatCount(estimates.uncostedRequests)} 次请求未设成本` : estimates ? '全部已设成本' : ''}</div></section>
    </div>

    {data?.byProvider && <section className="panel" aria-label="按供应商">
      <div className="panel-head"><h3>按供应商</h3><EstimateTag title={PROVIDER_ESTIMATE}/></div>
      <div className="table-scroll"><table className="table provider-costs">
        <thead><tr><th>供应商</th><th className="num">请求</th><th className="num">未缓存输入</th><th className="num">输出</th><th className="num">缓存读</th><th className="num">缓存写</th><th className="num">成本</th></tr></thead>
        <tbody>
          {byProvider.map(row => <tr key={row.providerId}>
            <td className="cell-strong" title={row.providerId}>{providerName(row.providerId)}</td>
            <td className="num">{formatCount(row.requests)}</td>
            <td className="num">{formatCount(row.uncachedInputTokens)}</td><td className="num">{formatCount(row.outputTokens)}</td>
            <td className="num">{formatCount(row.cacheReadTokens)}</td><td className="num">{formatCount(row.cacheWriteTokens)}</td>
            <td className="num" title={formatMoneyExact(row.costMicroCny)}>{row.costMicroCny || !row.requests ? formatMoney(row.costMicroCny) : <span className="is-warning" title="这些请求的线路没有设采购价">未设成本</span>}</td>
          </tr>)}
          {!byProvider.length && <TableState colSpan={7} loading={dataLoading} failed={dataFailed} empty="这段时间没有结算的请求" onRetry={() => void refresh()}/>}
        </tbody>
        {byProvider.length > 1 && <tfoot><tr><td>合计</td><td className="num">{formatCount(sum('requests'))}</td><td className="num">{formatCount(sum('uncachedInputTokens'))}</td>
          <td className="num">{formatCount(sum('outputTokens'))}</td><td className="num">{formatCount(sum('cacheReadTokens'))}</td><td className="num">{formatCount(sum('cacheWriteTokens'))}</td>
          <td className="num" title={formatMoneyExact(sum('costMicroCny'))}>{formatMoney(sum('costMicroCny'))}</td></tr></tfoot>}
      </table></div>
    </section>}

    <section className="panel" aria-label="按模型">
      <div className="panel-head"><h3>按模型</h3><EstimateTag title={ESTIMATE}/></div>
      <div className="table-scroll"><table className="table">
        <thead><tr><th>模型</th><th className="num">请求</th><th className="num">消耗积分</th><th className="num">面值</th><th className="num">采购成本</th><th className="num">毛利率</th></tr></thead>
        <tbody>
          {rankings.map((row, index) => {
            const credits = Number(row.credits_charged ?? 0);
            const faceValue = typeof face === 'number' && Number.isFinite(credits) ? credits * face : null;
            const cost = Number(row.provider_cost_micro_cny ?? 0);
            const unpriced = Number(row.requests ?? 0) > 0 && !cost;
            const rate = modelMargin(row, face);
            return <tr key={String(row.model_id ?? index)}>
              <td className="cell-strong">{String(row.model_id ?? '—')}</td>
              <td className="num">{formatCount(row.requests ?? 0)}</td>
              <td className="num">{row.credits_charged === undefined ? '—' : formatCreditsMicro(credits)}</td>
              <td className="num">{faceValue === null || row.credits_charged === undefined ? '—' : formatMoney(faceValue)}</td>
              <td className="num" title={cost ? formatMoneyExact(cost) : undefined}>{unpriced ? <span className="is-warning" title="这个模型的线路没有设采购价">未设成本</span> : Number(row.requests ?? 0) ? formatMoney(cost) : '—'}</td>
              <td className={`num${rate !== null && rate < 0 ? ' is-danger' : ''}`} title={rate !== null && rate < 0 ? '按成本在亏：售价低于采购价' : undefined}>{formatPercent(rate)}</td>
            </tr>;
          })}
          {!rankings.length && <TableState colSpan={6} loading={dataLoading} failed={dataFailed} empty="暂无数据" onRetry={() => void refresh()}/>}
        </tbody>
      </table></div>
    </section>

    {sales && <section className="panel" aria-label="销售">
      <div className="panel-head"><h3>销售</h3></div>
      <p className="sales-line">发卡 <b>{formatCount(sales.issuedCards)}</b> 张 · {formatMoney(sales.issuedValueMicroCny)} · 激活 <b>{formatCount(sales.activatedCards)}</b> 张 · {formatMoney(sales.activatedValueMicroCny)}
        <span className="muted">（按套餐价；作废且从未激活的卡不算）</span>
        {(sales.unpricedIssuedCards > 0 || sales.unpricedActivatedCards > 0) && <span className="is-warning"> · 另有发卡 {formatCount(sales.unpricedIssuedCards)} 张、激活 {formatCount(sales.unpricedActivatedCards)} 张不属于任何套餐，没有计价</span>}</p>
      {planRows.length > 0 ? <div className="table-scroll"><table className="table">
        <thead><tr><th>套餐</th><th className="num">积分</th><th className="num">套餐价</th><th className="num">发卡</th><th className="num">发卡金额</th><th className="num">激活</th><th className="num">激活金额</th></tr></thead>
        <tbody>{planRows.map(plan => <tr key={plan.planId ?? plan.templateId}>
          <td className="cell-strong">{plan.name}</td><td className="num">{formatCount(plan.points)}</td><td className="num">{formatMoney(plan.priceMicroCny)}</td>
          <td className="num">{formatCount(plan.issuedCards)}</td><td className="num">{formatMoney(plan.issuedValueMicroCny ?? plan.issuedCards * plan.priceMicroCny)}</td>
          <td className="num">{formatCount(plan.activatedCards)}</td><td className="num">{formatMoney(plan.activatedValueMicroCny ?? plan.activatedCards * plan.priceMicroCny)}</td>
        </tr>)}</tbody>
      </table></div> : <p className="muted">这段时间没有发卡或激活</p>}
    </section>}

    {liability && <section className="panel" aria-label="未消耗余额">
      <div className="panel-head"><h3>未消耗余额</h3><span className="muted">现在，不随统计区间变化</span></div>
      <p className="sales-line"><b>{formatCredits(liability.microCredits / 1_000_000)}</b> 积分 ≈ <b title={formatMoneyExact(liability.valueMicroCny)}>{formatMoney(liability.valueMicroCny)}</b>
        <span className="muted">（{formatCount(liability.cards)} 张卡，其中未激活 {formatCount(liability.unactivatedCards)} 张 · {formatCredits(liability.unactivatedMicroCredits / 1_000_000)} 积分）</span></p>
      <p className="muted">按当前积分面值折算客户还能用的余额：冻结的卡算在内，已到期、封禁、作废和归档的卡不算。</p>
    </section>}

    <FinancialPanel onPublished={() => refresh({keepSelection: true})} onDirtyChange={onDirtyChange} onBusyChange={onBusyChange} refreshEpoch={refreshEpoch}/>
  </div>;
}
