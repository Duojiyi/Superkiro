import { useState } from 'react';
import { finite, type Usage } from './bridge';

const points = (value: unknown) => finite(value) ? value.toLocaleString('zh-CN', { maximumFractionDigits: 6 }) : '—';

export function UsagePanel({ usage, state, refresh }: { usage: Usage|null; state: string; refresh: ()=>void }) {
  const [model, setModel] = useState('');
  const [page, setPage] = useState(0);
  const stats = usage?.settledUsage;
  const complete = finite(stats?.activatedAt) && finite(stats?.totalPoints);
  const models = stats?.models ?? [];
  const selected = models.find(row => row.name === model);
  const dailyByModel = new Map(selected?.daily?.map(row => [row.date, row.points]));
  const days = [...(stats?.daily ?? [])].reverse();
  const lastPage = Math.max(0, Math.ceil(days.length / 14) - 1);
  const currentPage = Math.min(page, lastPage);
  const unavailable = state !== 'ready' || !complete;
  return <section className="usage-panel">
    <div className="row"><h1>积分用量</h1><button title="刷新用量" aria-label="刷新用量" disabled={state==='loading'} onClick={refresh}>↻</button></div>
    {unavailable ? <p role="status" className="muted">{state==='loading'?'正在读取用量…':state==='disconnected'?'卡密登录后可查看用量。':state==='error'?'用量读取失败，请重试。':'自激活起的完整用量暂不可用，请确认网关已更新且历史结算明细完整。'}</p> : <>
      <p className="muted">激活于 {new Date(stats!.activatedAt! * 1000).toLocaleDateString('zh-CN', {timeZone:'UTC'})} · 按 UTC 日期结算</p>
      <div className="usage-totals"><div><span>累计消耗</span><strong>{points(stats!.totalPoints)} <small>积分</small></strong></div><div><span>今日消耗</span><strong>{points(stats!.todayPoints)} <small>积分</small></strong></div></div>
      <h2>各模型累计消耗</h2>
      {models.length ? <table><thead><tr><th>模型</th><th>积分</th></tr></thead><tbody>{models.map(row=><tr key={row.name}><td>{row.name}</td><td>{points(row.points)}</td></tr>)}</tbody></table> : <p className="muted">暂无已结算消耗</p>}
      <div className="row usage-daily-heading"><h2>每日消耗</h2><select aria-label="每日消耗模型" value={selected?.name ?? ''} onChange={event=>{setModel(event.target.value);setPage(0);}}><option value="">全部模型</option>{models.map(row=><option key={row.name} value={row.name}>{row.name}</option>)}</select></div>
      <table><thead><tr><th>日期</th><th>积分</th></tr></thead><tbody>{days.slice(currentPage*14, currentPage*14+14).map(row=><tr key={row.date}><td>{row.date}</td><td>{points(selected ? selected.daily ? dailyByModel.get(row.date) ?? 0 : undefined : row.points)}</td></tr>)}</tbody></table>
      {days.length>14&&<div className="row usage-pagination"><button aria-label="较新日期" title="较新日期" disabled={currentPage===0} onClick={()=>setPage(currentPage-1)}>←</button><span>{currentPage+1} / {lastPage+1}</span><button aria-label="较早日期" title="较早日期" disabled={currentPage===lastPage} onClick={()=>setPage(currentPage+1)}>→</button></div>}
    </>}
  </section>;
}
