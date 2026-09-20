import { finite, number, type Memory } from './bridge';

/** Ring segments are measured Kiro process groups, not invented system capacity. */
export function MemoryVisual({state,sample,samples}:{state:string;sample:Memory|null;samples:number[]}) {
  const ready=state==='ready'&&finite(sample?.total_memory_mb);
  const total=ready?sample!.total_memory_mb!:0;
  const classified=ready&&finite(sample?.ide_memory_mb)&&finite(sample?.agent_memory_mb)&&Math.abs(sample.ide_memory_mb+sample.agent_memory_mb-total)<=1;
  const share=classified&&total>0?Math.min(1,sample!.ide_memory_mb!/total):0;
  const label=state==='empty'?'未运行':state==='error'?'采样失败':state==='loading'?'采样中':'等待采样';
  return <div className="memory-visual">
    <div className="memory-ring">
      <svg viewBox="0 0 120 120" role="img" aria-label={ready?`Kiro 当前占用 ${number(total)} MB${classified?'，环形表示 Kiro 应用与关联子进程的内存分布':''}`:label}>
        <circle className="memory-ring-track" cx="60" cy="60" r="50"/>
        {classified&&total>0&&<><circle className="memory-ring-children" cx="60" cy="60" r="50"/><circle className="memory-ring-app" cx="60" cy="60" r="50" strokeDasharray={`${share*314.159} 314.159`} transform="rotate(-90 60 60)"/></>}
      </svg>
      <div className="memory-ring-label"><strong>{ready?number(total):label}</strong><span>{ready?'MB · 当前占用':'Kiro 内存'}</span></div>
    </div>
    <div className="memory-breakdown">
      <span className="memory-caption">Kiro 内存分布</span>
      {ready?<><div className="memory-legend"><span><i/>Kiro 应用</span><strong>{classified?`${number(sample!.ide_memory_mb)} MB`:'暂不可用'}</strong></div><div className="memory-legend children"><span><i/>关联子进程</span><strong>{classified?`${number(sample!.agent_memory_mb)} MB`:'暂不可用'}</strong></div><p className="muted">{finite(sample?.total_process_count)?`${sample.total_process_count} 个进程`:'进程数待确认'} · 非整机内存占比</p></>:<p className="muted">{state==='empty'?'启动 Kiro 后显示实际用量':state==='error'?'暂时无法读取，点击重新采样重试':'正在等待本地进程采样'}</p>}
      {ready&&samples.length>1&&<div className="memory-trend" role="img" aria-label="当前会话最近内存采样趋势">{samples.map((v,i)=><i key={i} style={{height:`${Math.max(3,v/Math.max(1,...samples)*100)}%`}}/>)}</div>}
    </div>
  </div>;
}
