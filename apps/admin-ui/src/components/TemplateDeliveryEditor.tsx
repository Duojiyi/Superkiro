import {newTemplateDelivery, type TemplateDelivery} from '../responseTemplates';
export function TemplateDeliveryEditor({value, onChange}: {value: TemplateDelivery | null | undefined; onChange: (v: TemplateDelivery | null) => void}) {
  const update = (patch: Partial<TemplateDelivery>) => value && onChange({...value, ...patch});
  return <section className="panel template-intro">
    <label><input type="checkbox" checked={!!value} onChange={e => onChange(e.target.checked ? newTemplateDelivery() : null)}/> 启用可编辑时间线与完整回执文案</label>
    {value && <>
      <p className="muted">时间均为请求开始后的绝对秒数。例如第 10 秒和第 40 秒发送，中间间隔 30 秒。每段最早/最晚相同时为固定时间，否则每次请求在范围内随机抽取一次；各段范围不能重叠。文件写入时间也独立随机抽取。等待中取消不扣费，文件指令下发时收费一次。不得把预设等待描述为模型实际思考。</p>
      <div className="form-grid form-grid-2">
        <label className="field"><span>最早写文件（秒）</span><input type="number" min="0" max="300" step="0.001" value={value.write_min_ms / 1000} onChange={e => update({write_min_ms: Math.round(Number(e.target.value) * 1000)})}/></label>
        <label className="field"><span>最晚写文件（秒）</span><input type="number" min="0" max="300" step="0.001" value={value.write_max_ms / 1000} onChange={e => update({write_max_ms: Math.round(Number(e.target.value) * 1000)})}/></label>
      </div>
      {value.messages.map((m, i) => <div className="form-grid form-grid-2" key={i}>
        <label className="field"><span>第 {i + 1} 段 · 最早发送（秒）</span><input type="number" min="0" max={value.write_min_ms / 1000} step="0.001" value={m.at_ms / 1000} onChange={e => update({messages: value.messages.map((v, j) => i === j ? {...v, at_ms: Math.round(Number(e.target.value) * 1000)} : v)})}/></label>
        <label className="field"><span>第 {i + 1} 段 · 最晚发送（秒）</span><input type="number" min={m.at_ms / 1000} max={value.write_min_ms / 1000} step="0.001" value={(m.at_max_ms ?? m.at_ms) / 1000} onChange={e => update({messages: value.messages.map((v, j) => i === j ? {...v, at_max_ms: Math.round(Number(e.target.value) * 1000) === v.at_ms ? undefined : Math.round(Number(e.target.value) * 1000)} : v)})}/></label>
        <label className="field"><span>发送文案</span><textarea value={m.text} onChange={e => update({messages: value.messages.map((v, j) => i === j ? {...v, text: e.target.value} : v)})}/></label>
        <button type="button" className="btn-text" onClick={() => update({messages: value.messages.filter((_, j) => i !== j)})}>删除第 {i + 1} 段</button>
      </div>)}
      <button type="button" className="btn" disabled={value.messages.length >= 16 || (value.messages[value.messages.length - 1]?.at_max_ms ?? value.messages[value.messages.length - 1]?.at_ms ?? -1) >= value.write_min_ms} onClick={() => update({messages: [...value.messages, {at_ms: Math.min(value.write_min_ms, (value.messages[value.messages.length - 1]?.at_max_ms ?? value.messages[value.messages.length - 1]?.at_ms ?? -10000) + 10000), text: ''}]})}>添加定时文案</button>
      <p className="muted">下列字段是完整文案，不再添加固定前缀。支持 {'{file_path}'}、{'{price}'}；成功仅在客户端确认后发送。文案不改变真实收费和用量记录。</p>
      <div className="form-grid form-grid-2">{([
        ['dispatch', '文件下发文案'], ['success', '写入成功回执'], ['failure', '写入失败回执'], ['unknown', '未知状态回执'], ['replay', '重试恢复文案'], ['continuation', '回执附带新指令提示'],
      ] as const).map(([key, label]) => <label className="field" key={key}><span>{label}</span><textarea value={value[key]} onChange={e => update({[key]: e.target.value})}/></label>)}</div>
    </>}
  </section>;
}
