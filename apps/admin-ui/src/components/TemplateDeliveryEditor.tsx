import {adjustTemplateSelection, MAX_TEMPLATE_MESSAGE_ALTERNATIVES, newTemplateDelivery, type TemplateDelivery} from '../responseTemplates';

export function TemplateDeliveryEditor({value, onChange}: {value: TemplateDelivery | null | undefined; onChange: (v: TemplateDelivery | null) => void}) {
  const update = (patch: Partial<TemplateDelivery>) => value && onChange({...value, ...patch});
  const patchMessage = (index: number, patch: object) => value && update({messages: value.messages.map((message, i) => i === index ? {...message, ...patch} : message)});
  return <section className="panel template-intro">
    <label><input type="checkbox" checked={!!value} onChange={e => onChange(e.target.checked ? newTemplateDelivery() : null)}/> 启用可编辑时间线与完整回执文案</label>
    {value && <>
      <p className="muted">时间均为请求开始后的绝对秒数。例如第 10 秒和第 40 秒发送，中间间隔 30 秒。每段最早/最晚相同时为固定时间，否则每次请求在范围内随机抽取一次；各段范围不能重叠。一个时间段是一个 slot，可配置多条候选文案并随机或指定发送其中一条。文件写入时间也独立随机抽取。等待中取消不扣费，文件指令下发时收费一次。不得把预设等待描述为模型实际思考。</p>
      <div className="form-grid form-grid-2">
        <label className="field"><span>最早写文件（秒）</span><input type="number" min="0" max="300" step="0.001" value={value.write_min_ms / 1000} onChange={e => update({write_min_ms: Math.round(Number(e.target.value) * 1000)})}/></label>
        <label className="field"><span>最晚写文件（秒）</span><input type="number" min="0" max="300" step="0.001" value={value.write_max_ms / 1000} onChange={e => update({write_max_ms: Math.round(Number(e.target.value) * 1000)})}/></label>
      </div>
      {value.messages.map((m, i) => {
        const alternatives = m.alternatives ?? [];
        return <div className="form-grid form-grid-2" key={i}>
          <label className="field"><span>第 {i + 1} 段 · 最早发送（秒）</span><input type="number" min="0" max={value.write_min_ms / 1000} step="0.001" value={m.at_ms / 1000} onChange={e => patchMessage(i, {at_ms: Math.round(Number(e.target.value) * 1000)})}/></label>
          <label className="field"><span>第 {i + 1} 段 · 最晚发送（秒）</span><input type="number" min={m.at_ms / 1000} max={value.write_min_ms / 1000} step="0.001" value={(m.at_max_ms ?? m.at_ms) / 1000} onChange={e => patchMessage(i, {at_max_ms: Math.round(Number(e.target.value) * 1000) === m.at_ms ? undefined : Math.round(Number(e.target.value) * 1000)})}/></label>
          <label className="field"><span>第 {i + 1} 段 · 选择方式</span><select value={m.selected_index == null ? '' : String(m.selected_index)} onChange={e => patchMessage(i, {selected_index: e.target.value === '' ? undefined : Number(e.target.value)})}><option value="">随机选择（多条候选）</option>{[m.text, ...alternatives].map((_, candidate) => <option key={candidate} value={candidate}>指定第 {candidate + 1} 条</option>)}</select></label>
          <label className="field"><span>第 {i + 1} 段 · 候选 1</span><textarea value={m.text} onChange={e => patchMessage(i, {text: e.target.value})}/></label>
          {alternatives.map((text, alternative) => <div className="field" key={alternative}><label className="field"><span>第 {i + 1} 段 · 候选 {alternative + 2}</span><textarea value={text} onChange={e => patchMessage(i, {alternatives: alternatives.map((item, j) => j === alternative ? e.target.value : item)})}/></label><button type="button" className="btn-text" onClick={() => patchMessage(i, {alternatives: alternatives.filter((_, j) => j !== alternative), selected_index: adjustTemplateSelection(m.selected_index, alternative + 1)})}>删除此候选</button></div>)}
          <div className="button-row"><button type="button" className="btn" disabled={alternatives.length >= MAX_TEMPLATE_MESSAGE_ALTERNATIVES} onClick={() => patchMessage(i, {alternatives: [...alternatives, '']})}>新增候选文案</button><button type="button" className="btn-text" onClick={() => update({messages: value.messages.filter((_, j) => i !== j)})}>删除第 {i + 1} 段</button></div>
        </div>;
      })}
      <button type="button" className="btn" disabled={value.messages.length >= 16 || (value.messages[value.messages.length - 1]?.at_max_ms ?? value.messages[value.messages.length - 1]?.at_ms ?? -1) >= value.write_min_ms} onClick={() => update({messages: [...value.messages, {at_ms: Math.min(value.write_min_ms, (value.messages[value.messages.length - 1]?.at_max_ms ?? value.messages[value.messages.length - 1]?.at_ms ?? -10000) + 10000), text: '', alternatives: []}]})}>添加定时文案 slot</button>
      <p className="muted">下列字段是完整文案，不再添加固定前缀。支持 {'{file_path}'}、{'{price}'}；成功仅在客户端确认后发送。文案不改变真实收费和用量记录。</p>
      <div className="form-grid form-grid-2">{([
        ['dispatch', '文件下发文案'], ['success', '写入成功回执'], ['failure', '写入失败回执'], ['unknown', '未知状态回执'], ['replay', '重试恢复文案'], ['continuation', '回执附带新指令提示'],
      ] as const).map(([key, label]) => <label className="field" key={key}><span>{label}</span><textarea value={value[key]} onChange={e => update({[key]: e.target.value})}/></label>)}</div>
    </>}
  </section>;
}
