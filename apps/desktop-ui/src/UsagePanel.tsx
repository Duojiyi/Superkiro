import { useState } from 'react';
import { finite, type Usage } from './bridge';

const points = (value: unknown) => finite(value) ? value.toLocaleString('zh-CN', { maximumFractionDigits: 6 }) : '—';
const colors = ['#b9d6c3', '#dfc69b', '#b4b4db', '#91becb', '#d6aaac'];

export function UsagePanel({ usage, state, refresh }: { usage: Usage|null; state: string; refresh: ()=>void }) {
  const [model, setModel] = useState('');
  const [page, setPage] = useState(0);
  const stats = usage?.settledUsage;
  const complete = finite(stats?.activatedAt) && finite(stats?.totalPoints);
  const models = [...(stats?.models ?? [])].sort((a, b) => (b.points ?? 0) - (a.points ?? 0));
  const selected = models.find(row => row.name === model);
  const dailyByModel = new Map(selected?.daily?.map(row => [row.date, row.points]));
  const days = [...(stats?.daily ?? [])].sort((a, b) => b.date.localeCompare(a.date));
  const lastPage = Math.max(0, Math.ceil(days.length / 14) - 1);
  const currentPage = Math.min(page, lastPage);
  const visibleDays = days.slice(currentPage * 14, currentPage * 14 + 14).map(row => ({
    date: row.date, points: selected ? selected.daily ? dailyByModel.get(row.date) ?? 0 : undefined : row.points,
  }));
  const chartDays = [...visibleDays].reverse();
  const chartComplete = chartDays.length > 0 && chartDays.every(row => finite(row.points) && row.points >= 0);
  const peak = Math.max(0, ...chartDays.map(row => finite(row.points) ? row.points : 0));
  const modelTotal = models.reduce((sum, row) => sum + (finite(row.points) && row.points > 0 ? row.points : 0), 0);
  const unavailable = state !== 'ready' || !complete;
  return <section className="usage-panel">
    <div className="row usage-title"><div><span className="usage-eyebrow">POINTS OVERVIEW</span><h1>积分用量</h1></div><button className="usage-refresh" title="刷新用量" aria-label="刷新用量" disabled={state==='loading'} onClick={refresh}>↻</button></div>
    {unavailable ? <div className="usage-empty" role="status"><span aria-hidden="true">▥</span><p>{state==='loading'?'正在读取用量…':state==='disconnected'?'卡密登录后可查看用量。':state==='error'?'用量读取失败，请重试。':'自激活起的完整用量暂不可用，请确认网关已更新且历史结算明细完整。'}</p></div> : <>
      <p className="usage-caption">激活于 {new Date(stats!.activatedAt! * 1000).toLocaleDateString('zh-CN', {timeZone:'UTC'})} · 按 UTC 日期结算</p>
      <div className="usage-totals"><div className="usage-total-primary"><span>累计消耗</span><strong>{points(stats!.totalPoints)}</strong><small>积分 · 自激活起</small></div><div><span>今日消耗</span><strong>{points(stats!.todayPoints)}</strong><small>积分 · 已结算</small></div></div>
      <div className="usage-card">
        <div className="row usage-daily-heading"><div><h2>每日消耗</h2><p className="usage-caption">{selected?.name ?? '全部模型'} · 每页最多 14 天</p></div><select aria-label="每日消耗模型" value={selected?.name ?? ''} onChange={event=>{setModel(event.target.value);setPage(0);}}><option value="">全部模型</option>{models.map(row=><option key={row.name} value={row.name}>{row.name}</option>)}</select></div>
        {chartComplete ? <div className="usage-trend"><div className="row usage-trend-scale"><span>积分</span><span>单日最高 {points(peak)}</span></div><svg viewBox="0 0 420 124" role="img" aria-label="每日积分消耗趋势"><title>每日积分消耗趋势，精确值可在每日明细查看</title>{[12, 46, 80, 114].map(y=><line key={y} x1="0" x2="420" y1={y} y2={y} className="usage-grid-line"/>)}{chartDays.map((row, index)=>{const slot=420/chartDays.length; const height=peak>0?row.points!/peak*100:0;return <g key={row.date}><rect x={index*slot+slot*.18} y={114-height} width={slot*.64} height={height||1.5} rx="3" className={height>0?'usage-bar':'usage-bar-zero'} tabIndex={0} aria-label={`${row.date}：${points(row.points)} 积分`}><title>{row.date} · {points(row.points)} 积分</title></rect></g>;})}</svg><div className="row usage-trend-dates"><span>{chartDays[0].date.slice(5)}</span><span>{chartDays.length>1?chartDays[chartDays.length-1].date.slice(5):''}</span></div></div> : <p className="usage-empty-caption">{chartDays.length?'此模型的每日明细尚不完整，暂不绘制趋势。':'暂无已结算的每日消耗'}</p>}
        {days.length>14&&<div className="row usage-pagination"><button aria-label="较新日期" title="较新日期" disabled={currentPage===0} onClick={()=>setPage(currentPage-1)}>←</button><span>第 {currentPage+1} / {lastPage+1} 页</span><button aria-label="较早日期" title="较早日期" disabled={currentPage===lastPage} onClick={()=>setPage(currentPage+1)}>→</button></div>}
        {visibleDays.length>0&&<details className="usage-details"><summary>查看每日明细<span>积分</span></summary><table><thead><tr><th>日期（UTC）</th><th>消耗积分</th></tr></thead><tbody>{visibleDays.map(row=><tr key={row.date}><td>{row.date}</td><td>{points(row.points)}</td></tr>)}</tbody></table></details>}
      </div>
      <div className="usage-card"><div className="row"><h2>各模型累计消耗</h2><span className="usage-count">{models.length} 个模型</span></div><p className="usage-caption">自激活起 · 选择上方模型可查看每日趋势</p>
        {models.length ? <ul className="usage-models">{models.map((row,index)=>{const percent=finite(row.points)&&row.points>=0&&modelTotal>0?row.points/modelTotal*100:0;return <li key={row.name}><div className="row usage-model-label"><span className="usage-model-name"><i style={{background:colors[index%colors.length]}}/>{row.name}</span><strong>{points(row.points)} <small>积分</small></strong></div><div className="usage-model-meter" aria-hidden="true"><span style={{width:`${percent}%`,background:colors[index%colors.length]}}/></div></li>;})}</ul> : <p className="usage-empty-caption">暂无已结算消耗</p>}
      </div>
      <p className="usage-footnote">仅统计已结算积分，进行中的请求将在结算后计入。</p>
    </>}
  </section>;
}
