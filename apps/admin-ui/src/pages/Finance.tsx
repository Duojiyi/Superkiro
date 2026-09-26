// 财务对账: what the kept usage ledger adds up to, by model, and the two settings that turn
// credits and USD prices into yuan. Every yuan figure here is an estimate from the ledger.
import {useRef} from 'react';
import {adminApi, type AdminFinancials} from '../api';
import {Menu} from '../components/menu';
import {toast} from '../components/toast';
import {EstimateTag, TableState, TopbarActions} from '../components/ui';
import FinancialPanel from '../FinancialPanel';
import {financialEstimates} from '../financial';
import {formatCount, formatCreditsMicro, formatMoney, formatMoneyExact, formatPercent} from '../format';
import type {Refresh, ReportError} from '../types';

const ESTIMATE = '基于保留的用量账本估算，不含实际收款与发票';

export default function FinancePage({financials, loading, failed, refresh, reportError, onDirtyChange, onBusyChange}: {
  financials: AdminFinancials | null;
  loading: boolean;
  failed: boolean;
  refresh: Refresh;
  reportError: ReportError;
  onDirtyChange: (dirty: boolean) => void;
  onBusyChange: (busy: boolean) => void;
}) {
  const exporting = useRef(false);
  const exportLedger = async (format: 'json' | 'csv') => {
    if (exporting.current) return;
    exporting.current = true;
    try {
      toast.info('正在导出…');
      const blob = await adminApi.exportLedger(format);
      const url = window.URL.createObjectURL(blob);
      const link = document.createElement('a');
      link.href = url;
      link.download = `ledger-export-${new Date().toISOString().slice(0, 10)}.${format}`;
      document.body.appendChild(link);
      link.click();
      link.remove();
      // The browser reads the file after click() returns; revoking at once can cut it.
      setTimeout(() => window.URL.revokeObjectURL(url), 60000);
      toast.success('已开始下载，请在浏览器下载列表中确认文件完整');
    } catch (error) {
      reportError(`导出失败：${error instanceof Error ? error.message : String(error)}`);
    } finally {exporting.current = false;}
  };

  const estimates = financialEstimates(financials);
  const face = financials?.settings?.credit_face_value_cny;
  const dashboard = financials?.dashboard;
  const covered = estimates ? estimates.costedRequests : null;
  const total = estimates ? estimates.costedRequests + estimates.uncostedRequests : null;
  const rankings = financials?.modelRankings ?? [];

  return <div className="page-stack">
    <TopbarActions>
      <button type="button" className="btn" onClick={() => void exportLedger('csv')}>导出 CSV</button>
      <Menu label="更多导出" className="btn btn-icon-only" items={[{label: '导出 JSON', onSelect: () => void exportLedger('json')}]}/>
    </TopbarActions>

    <div className="kpi-row">
      <section className="panel kpi"><p className="kpi-label">消耗积分</p>
        <strong className="kpi-value">{dashboard ? formatCreditsMicro(dashboard.total_credits_charged) : '—'}</strong>
        <div className="kpi-sub">{dashboard ? `${formatCount(dashboard.total_requests)} 次结算` : ''}</div></section>
      <section className="panel kpi"><p className="kpi-label">面值收入<EstimateTag title={ESTIMATE}/></p>
        <strong className="kpi-value" title={estimates ? formatMoneyExact(estimates.usageFaceValueMicroCny) : undefined}>{estimates ? formatMoney(estimates.usageFaceValueMicroCny) : '—'}</strong>
        <div className="kpi-sub">{typeof face === 'number' ? `积分面值 ${face} 元/积分` : ''}</div></section>
      <section className="panel kpi"><p className="kpi-label">采购成本<EstimateTag title={ESTIMATE}/></p>
        <strong className="kpi-value" title={estimates ? formatMoneyExact(estimates.configuredProviderCostMicroCny) : undefined}>{estimates ? formatMoney(estimates.configuredProviderCostMicroCny) : '—'}</strong>
        <div className="kpi-sub">{estimates?.uncostedRequests ? '部分请求未定价' : ''}</div></section>
      <section className="panel kpi"><p className="kpi-label">毛利<EstimateTag title={ESTIMATE}/></p>
        {estimates && estimates.uncostedRequests > 0
          ? <><strong className="kpi-value">—</strong><div className="kpi-sub is-warning">{formatCount(estimates.uncostedRequests)} 次请求未定价，毛利暂不计算</div></>
          : <><strong className="kpi-value" title={estimates ? formatMoneyExact(estimates.faceValueLessCostMicroCny) : undefined}>{estimates ? formatMoney(estimates.faceValueLessCostMicroCny) : '—'}</strong>
            <div className="kpi-sub">{estimates && typeof estimates.faceValueMarginPercentage === 'number' ? `毛利率 ${formatPercent(estimates.faceValueMarginPercentage)}` : ''}</div></>}
      </section>
      <section className="panel kpi"><p className="kpi-label">成本覆盖</p>
        <strong className="kpi-value">{covered !== null && total !== null ? `${formatCount(covered)} / ${formatCount(total)}` : '—'}</strong>
        <div className="kpi-sub">{estimates?.uncostedRequests ? `${formatCount(estimates.uncostedRequests)} 次请求未定价` : estimates ? '全部已定价' : ''}</div></section>
    </div>

    <section className="panel">
      <div className="panel-head"><h3>按模型</h3><EstimateTag title={ESTIMATE}/></div>
      <div className="table-scroll"><table className="table">
        <thead><tr><th>模型</th><th className="num">请求</th><th className="num">消耗积分</th><th className="num">面值</th><th className="num">采购成本</th><th className="num">毛利率</th></tr></thead>
        <tbody>
          {rankings.map((row, index) => {
            const credits = Number(row.credits_charged ?? 0);
            const faceValue = typeof face === 'number' && Number.isFinite(credits) ? credits * face : null;
            const cost = Number(row.provider_cost_micro_cny ?? 0);
            const unpriced = Number(row.requests ?? 0) > 0 && !cost;
            // Face value against configured cost, the same basis as the totals above.
            const margin = !unpriced && faceValue ? (faceValue - cost) / faceValue * 100 : null;
            return <tr key={String(row.model_id ?? index)}>
              <td className="cell-strong">{String(row.model_id ?? '—')}</td>
              <td className="num">{formatCount(row.requests ?? 0)}</td>
              <td className="num">{row.credits_charged === undefined ? '—' : formatCreditsMicro(credits)}</td>
              <td className="num">{faceValue === null || row.credits_charged === undefined ? '—' : formatMoney(faceValue)}</td>
              <td className="num" title={cost ? formatMoneyExact(cost) : undefined}>{unpriced ? <span className="is-warning">未定价</span> : Number(row.requests ?? 0) ? formatMoney(cost) : '—'}</td>
              <td className="num">{formatPercent(margin)}</td>
            </tr>;
          })}
          {!rankings.length && <TableState colSpan={6} loading={loading} failed={failed} empty="暂无数据" onRetry={() => void refresh()}/>}
        </tbody>
      </table></div>
    </section>

    <FinancialPanel onPublished={() => refresh({keepSelection: true})} onDirtyChange={onDirtyChange} onBusyChange={onBusyChange}/>
  </div>;
}
