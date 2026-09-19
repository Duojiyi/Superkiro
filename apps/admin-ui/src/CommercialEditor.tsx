import { pointsToMicro } from './pricing';
import { useEffect, useState } from 'react';
import { adminApi, CommercialConfig } from './api';

export default function CommercialEditor({ kind }: {kind: 'groups' | 'models'}) {
  const [config, setConfig] = useState<CommercialConfig | null>(null);
  const [draft, setDraft] = useState('');
  const [reason, setReason] = useState('');
  const [message, setMessage] = useState('');
  const [busy, setBusy] = useState(false);
  const [selected, setSelected] = useState(0);
  const [priceDraft, setPriceDraft] = useState<Record<string, unknown> | null>(null);
  const [priceInputs, setPriceInputs] = useState<Record<string, string>>({});
  const priceFields = {fixed_input_credit_per_m: '输入', fixed_output_credit_per_m: '输出', fixed_cache_read_credit_per_m: '缓存读取', fixed_cache_creation_credit_per_m: '缓存写入'};
  const stagePrice = () => {
    try {
      if (!priceDraft || !String(priceDraft.id ?? '').trim()) throw new Error('请填写新的价格版本 ID');
      if (config?.versions.some(v => v.id === priceDraft.id)) throw new Error('版本 ID 已存在，不能覆盖历史价格');
      if (!Number.isFinite(Number(priceDraft.effective_from_secs)) || Number(priceDraft.effective_from_secs) <= 0) throw new Error('请填写有效生效时间');
      const values = Object.fromEntries(Object.keys(priceFields).map(field => [field, pointsToMicro(priceInputs[field] ?? '')]));
      const versions = [...(Array.isArray(parsedDraft.versions) ? parsedDraft.versions : []).filter(v => v.id !== priceDraft.id), Object.fromEntries(Object.entries({...priceDraft, ...values}).filter(([key]) => key !== 'source_id'))];
      setDraft(JSON.stringify({...parsedDraft, versions}, null, 2)); setMessage('价格版本已加入本页草稿，尚未发布。请填写原因并确认发布。');
    } catch (error) { setMessage(String(error)); }
  };
  let parsedDraft: Record<string, Array<Record<string, unknown>>> = {};
  try { const value = JSON.parse(draft); if (value && typeof value === 'object' && !Array.isArray(value)) parsedDraft = value; } catch { /* Keep invalid advanced edits visible for correction. */ }
  const rows = Array.isArray(parsedDraft[kind]) ? parsedDraft[kind].filter(row => row && typeof row === 'object' && !Array.isArray(row)) : [];
  const selectedRow = rows[selected];
  const updateField = (field: string, value: unknown) => {
    const next = rows.map((row, index) => index === selected ? {...row, [field]: value} : row);
    setDraft(JSON.stringify({...parsedDraft, [kind]: next}, null, 2));
  };
  const fields = kind === 'groups' ? ['name', 'virtual_plan_name', 'virtual_usage_limit', 'rate_card_id', 'margin_multiplier'] : ['exposed_model_id', 'target_provider_id', 'target_model', 'group_id', 'context_window', 'max_output', 'credit_multiplier', 'visible', 'supports_tools', 'supports_vision', 'supports_reasoning'];
  const labels: Record<string, string> = {name: '分组名称', virtual_plan_name: '套餐名称', virtual_usage_limit: '虚拟用量上限（非发卡积分）', rate_card_id: '价格表 ID', margin_multiplier: '倍率', exposed_model_id: '展示模型 ID', target_provider_id: '供应商 ID', target_model: '上游模型 ID', group_id: '分组 ID', context_window: '上下文长度', max_output: '最大输出', credit_multiplier: '积分倍率', visible: '发布到用户目录', supports_tools: '工具调用', supports_vision: '视觉', supports_reasoning: '推理'};
  const load = async () => {
    setBusy(true);
    try {
      const result = await adminApi.getCommercialConfig();
      setConfig(result.config); setSelected(0); setPriceDraft(null);
      setDraft(JSON.stringify(kind === 'groups' ? {groups: result.config.groups} : {models: result.config.models, rate_cards: result.config.rate_cards, versions: []}, null, 2));
      setMessage('已读取当前配置。新增价格版本请放入 versions；既有版本只能查看，不可覆盖。');
    } catch (e) { setMessage(String(e)); } finally { setBusy(false); }
  };
  useEffect(() => { void load(); }, [kind]);
  const publish = async () => {
    if (!config || busy) return;
    try {
      const parsed: unknown = JSON.parse(draft);
      if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error('配置必须是 JSON 对象');
      if (!reason.trim()) throw new Error('请填写变更原因');
      if (!window.confirm('确认发布配置？新请求将使用新映射，价格按生效时间启用。有未结算请求时发布会被拒绝。')) return;
      setBusy(true);
      const result = await adminApi.publishCommercialConfig({...parsed, expected_revision: config.revision, reason});
      setConfig(result.config); setReason(''); setPriceDraft(null);
      setDraft(JSON.stringify(kind === 'groups' ? {groups: result.config.groups} : {models: result.config.models, rate_cards: result.config.rate_cards, versions: []}, null, 2));
      setMessage('发布成功，配置与审计记录已保存。');
    } catch (e) { setMessage(String(e)); } finally { setBusy(false); }
  };
  return <div className="space-y-6">
    {kind === 'models' && <div className="step-bar"><span>01 选择模型</span><span>02 映射路由</span><span>03 积分价格</span><span>04 校验发布</span></div>}
    <section className="panel"><h3>{kind === 'groups' ? '套餐分组' : '模型目录与路由'}</h3><p className="muted">{kind === 'groups' ? '发卡套餐：PRO 1,000 · PRO+ 2,000 · PRO Max 5,000 · Power 10,000。单卡单设备；实际发卡权益由服务端校验。' : '模型发现不等于 Key 授权。请核对可用路由和价格后发布。'}</p>
      <table><thead><tr>{(kind === 'groups' ? ['分组', '套餐', '价格表', '操作'] : ['展示名称', '上游模型 ID', '供应商 / Key 池', '操作']).map(label => <th key={label}>{label}</th>)}</tr></thead><tbody>{rows.map((row, index) => <tr key={String(row.id ?? index)} className={selected === index ? 'selected-row' : ''}><td>{String(row[kind === 'groups' ? 'name' : 'exposed_model_id'] ?? row.id)}</td><td>{String(row[kind === 'groups' ? 'virtual_plan_name' : 'target_model'] ?? '—')}</td><td>{String(row[kind === 'groups' ? 'rate_card_id' : 'target_provider_id'] ?? '—')}</td><td><button onClick={() => setSelected(index)}>编辑配置 →</button></td></tr>)}{!rows.length && <tr><td colSpan={4} className="empty-state">暂无已读取的配置</td></tr>}</tbody></table>
    </section>
    <div className="two-columns"><section className="panel"><h3>正在编辑 · {String(selectedRow?.name ?? selectedRow?.exposed_model_id ?? '请选择条目')}</h3><fieldset disabled={busy || !selectedRow} className="field-grid">{selectedRow && fields.filter(field => field in selectedRow).map(field => <label key={field}>{labels[field]}{typeof selectedRow[field] === 'boolean' ? <input type="checkbox" checked={Boolean(selectedRow[field])} onChange={e => updateField(field, e.target.checked)} /> : <input type={typeof selectedRow[field] === 'number' ? 'number' : 'text'} step="any" value={String(selectedRow[field] ?? '')} onChange={e => updateField(field, typeof selectedRow[field] === 'number' ? Number(e.target.value) : e.target.value)} />}</label>)}</fieldset></section><section className="panel"><h3>发布前检查</h3><p className="muted">版本：{config?.revision ?? '尚未读取'}</p><p>发布需要变更原因和二次确认。服务端校验版本冲突、路由与在途请求。</p><p className="muted">配置更新不代表存量卡密权益已变更。既有价格版本不可覆盖，新版本按生效时间启用。</p></section></div>
    {kind === 'models' && <div className="two-columns"><section className="panel"><h3>积分价格</h3><p className="muted">单位：积分 / 百万 Tokens。仅固定积分模式可在此编辑；成本加成与按次计费保留高级配置。</p><select aria-label="选择价格版本模板" value={String(priceDraft?.source_id ?? '')} onChange={e => {const version = config?.versions.find(v => v.id === e.target.value); if (!version) {setPriceDraft(null); return;} setPriceDraft({...version, id: '', effective_from_secs: 0, source_id: version.id}); setPriceInputs(Object.fromEntries(Object.keys(priceFields).map(field => [field, String(Number(version[field] ?? 0) / 1_000_000)])));}}><option value="">从现有版本创建新草稿</option>{config?.versions.filter(v => v.pricing_mode === 'fixed').map(v => <option key={String(v.id)} value={String(v.id)}>{String(v.model)} · {String(v.id)}</option>)}</select>{priceDraft && <div className="field-grid page-supplement"><label>新版本 ID<input value={String(priceDraft.id)} onChange={e => setPriceDraft({...priceDraft, id: e.target.value})}/></label><label>生效时间（本地时区）<input type="datetime-local" onChange={e => setPriceDraft({...priceDraft, effective_from_secs: Math.floor(new Date(e.target.value).getTime()/1000)})}/></label>{Object.entries(priceFields).map(([field, label]) => <label key={field}>{label}<input inputMode="decimal" value={priceInputs[field] ?? ''} onChange={e => setPriceInputs({...priceInputs, [field]: e.target.value})}/></label>)}<button className="primary" onClick={stagePrice}>加入价格草稿</button></div>}</section><section className="notice-panel"><h3>价格版本与生效规则</h3><p>积分以精确微积分提交，1 积分 = 1,000,000 微积分。</p><p>历史版本保持只读。新增版本沿用所选版本的模型、价格表与采购字段；发布前请核对高级配置。</p><p className="muted">页面草稿不做浏览器持久化。尚未提供价格模板时，请通过高级配置添加首个版本。</p></section></div>}
    <section className="p-6 rounded-xl bg-white border border-[#E5E8E5] space-y-4">
    <h3 className="font-semibold text-[#23272B]">{kind === 'groups' ? '分组配置' : '模型映射与版本化定价'}</h3>
    <p className="text-[#7B8388] text-sm">配置编辑器只提交列出的条目，不会因省略条目而删除配置。隐藏模型请设置 visible=false。积分价格字段以微积分为单位，1 积分 = 1,000,000 微积分。</p>
    <p role="status" className="text-[#A87029] text-sm">{message}</p>
    <button disabled={busy} onClick={() => { if (!config || window.confirm('重新读取将丢弃未发布的编辑，继续吗？')) void load(); }} className="px-4 py-2 bg-[#EFF1EF] rounded">重新读取配置</button>
    <details><summary>高级配置 JSON · 新增条目与价格版本</summary><label className="block">配置 JSON<textarea aria-label="配置 JSON" value={draft} disabled={busy} onChange={e => setDraft(e.target.value)} spellCheck={false} className="block w-full h-96 bg-white font-mono text-sm p-3 border border-[#E5E8E5] rounded" /></label></details>
    <label className="block">变更原因<input aria-label="变更原因" value={reason} maxLength={500} disabled={busy} onChange={e=>setReason(e.target.value)} className="block w-full bg-white p-3 border border-[#E5E8E5] rounded" /></label>
    <button disabled={busy || !config || !reason.trim()} onClick={()=>void publish()} className="px-4 py-2 rounded bg-[#B94B39] text-white disabled:opacity-50">{busy ? '处理中…' : '确认并发布'}</button>
    <details><summary>有效配置与历史版本（只读）</summary><pre className="overflow-auto text-xs p-3">{JSON.stringify(config,null,2)}</pre></details>
  </section></div>;
}
