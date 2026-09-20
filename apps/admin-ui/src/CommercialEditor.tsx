import { priceToMicroPerMillion, formatMicroPrice, previewFixedCharge, type PriceUnit } from './pricing';
import { useEffect, useState } from 'react';
import { adminApi, CommercialConfig } from './api';

export default function CommercialEditor({ kind,onDirtyChange,onBusyChange }: {kind: 'groups' | 'models';onDirtyChange:(dirty:boolean)=>void;onBusyChange:(busy:boolean)=>void}) {
  const [config, setConfig] = useState<CommercialConfig | null>(null);
  const [draft, setDraft] = useState(''),[loadedDraft,setLoadedDraft]=useState('');
  const [reason, setReason] = useState('');
  const [message, setMessage] = useState('');
  const [busy, setBusy] = useState(false);
  useEffect(()=>{onBusyChange(busy);return()=>onBusyChange(false);},[busy,onBusyChange]);
  const [selected, setSelected] = useState(0);
  const [priceDraft, setPriceDraft] = useState<Record<string, unknown> | null>(null);
  const [priceInputs, setPriceInputs] = useState<Record<string, string>>({});
  useEffect(()=>{onDirtyChange(draft!==loadedDraft||!!reason.trim()||!!priceDraft);},[draft,loadedDraft,reason,priceDraft,onDirtyChange]);
  const [priceUnit, setPriceUnit] = useState<PriceUnit>('million');
  const [tokenInputs, setTokenInputs] = useState(['1000', '1000', '0', '0']);
  const costFields={input_price_per_m:'采购输入价格',output_price_per_m:'采购输出价格',cache_read_price_per_m:'采购缓存读取价格',cache_creation_price_per_m:'采购缓存写入价格'};
  const priceFields = {fixed_input_credit_per_m: '未缓存输入', fixed_output_credit_per_m: '输出', fixed_cache_read_credit_per_m: '缓存读取', fixed_cache_creation_credit_per_m: '缓存写入'};
  const stagePrice = () => {
    try {
      if (!priceDraft || !String(priceDraft.id ?? '').trim()) throw new Error('请填写新的价格版本 ID');
      if (config?.versions.some(v => v.id === priceDraft.id)) throw new Error('版本 ID 已存在，不能覆盖历史价格');
      if (!Number.isFinite(Number(priceDraft.effective_from_secs)) || Number(priceDraft.effective_from_secs) <= 0) throw new Error('请填写有效生效时间');
      if(!['USD','CNY'].includes(String(priceDraft.currency)))throw new Error('请选择采购计价币种');
      if (String(priceDraft.margin_multiplier ?? '').trim() === '' || !Number.isFinite(Number(priceDraft.margin_multiplier)) || Number(priceDraft.margin_multiplier) < 0) throw new Error('价格版本倍率必须为非负有限数');
      const costs=Object.fromEntries(Object.keys(costFields).map(field=>{const raw=String(priceDraft[field]??'');const value=Number(raw);if(!raw.trim()||!Number.isFinite(value)||value<0)throw new Error('四类采购价格必须为非负有限数，明确免费时才填 0');return [field,value];}));
      const values = Object.fromEntries(Object.keys(priceFields).map(field => [field, priceToMicroPerMillion(priceInputs[field] ?? '', priceUnit)]));
      const versions = [...(Array.isArray(parsedDraft.versions) ? parsedDraft.versions : []).filter(v => v.id !== priceDraft.id), Object.fromEntries(Object.entries({...priceDraft, margin_multiplier: Number(priceDraft.margin_multiplier), ...values, ...costs}).filter(([key]) => key !== 'source_id'))];
      setDraft(JSON.stringify({...parsedDraft, versions}, null, 2)); setPriceDraft(null); setPriceInputs({}); setMessage('价格版本已加入本页草稿，尚未发布。请填写原因并确认发布。');
    } catch (error) { setMessage(String(error)); }
  };
  let parsedDraft: Record<string, Array<Record<string, unknown>>> = {};
  try { const value = JSON.parse(draft); if (value && typeof value === 'object' && !Array.isArray(value)) parsedDraft = value; } catch { /* Keep invalid advanced edits visible for correction. */ }
  const rows = Array.isArray(parsedDraft[kind]) ? parsedDraft[kind].filter(row => row && typeof row === 'object' && !Array.isArray(row)) : [];
  const selectedRow = rows[selected];
  const previewFields = ['fixed_input_credit_per_m', 'fixed_output_credit_per_m', 'fixed_cache_creation_credit_per_m', 'fixed_cache_read_credit_per_m'];
  const previewGroup = config?.groups.find(group => group.id === selectedRow?.group_id);
  let preview = '', previewError = '';
  if (priceDraft) {
    try {
      if (!selectedRow || !previewGroup) throw new Error('请选择模型并确认其所属分组');
      if (previewGroup.rate_card_id !== priceDraft.rate_card_id) throw new Error('当前模型的分组价格表与此草稿不匹配，请选择对应模型');
      if (![selectedRow.exposed_model_id, selectedRow.target_model, '*'].includes(priceDraft.model)) throw new Error('此价格草稿不匹配当前模型，暂不预览');
      if (String(priceDraft.margin_multiplier ?? '').trim() === '') throw new Error('请填写价格版本倍率');
      const charge = previewFixedCharge(previewFields.map(field => priceToMicroPerMillion(priceInputs[field] ?? '', priceUnit)), tokenInputs,
        [Number(priceDraft.margin_multiplier), Number(previewGroup.margin_multiplier), Number(selectedRow.credit_multiplier)]);
      preview = formatMicroPrice(charge) + ' 积分（' + charge + ' 微积分）';
    } catch (error) { previewError = error instanceof Error ? error.message : String(error); }
  }
  const convertedPrice = (field: string) => {
    try {
      const micro = priceToMicroPerMillion(priceInputs[field] ?? '', priceUnit);
      return formatMicroPrice(micro) + ' 积分 / 百万 Tokens = ' + micro + ' 微积分 / 百万 Tokens（倍率前）';
    } catch { return '请填写有效售价；空白不按免费处理'; }
  };
  const historyCredits = (value: unknown) => {
    try {
      if (typeof value !== 'number') throw new Error('missing price');
      return formatMicroPrice(value);
    } catch { return '无法安全显示，请核对原始配置'; }
  };
  const historyTime = (value: unknown) => {
    if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0) return '未提供有效时间';
    const date = new Date(value * 1000);
    return Number.isFinite(date.getTime()) ? date.toLocaleString() : '未提供有效时间';
  };
  const changePriceUnit = (unit: PriceUnit) => {
    try {
      const next = Object.fromEntries(Object.entries(priceInputs).map(([field, value]) => [field, formatMicroPrice(priceToMicroPerMillion(value, priceUnit), unit)]));
      setPriceInputs(next); setPriceUnit(unit);
    } catch (error) { setMessage('切换单位前请修正售价：' + String(error)); }
  };
  const updateField = (field: string, value: unknown) => {
    const next = rows.map((row, index) => index === selected ? {...row, [field]: value} : row);
    setDraft(JSON.stringify({...parsedDraft, [kind]: next}, null, 2));
  };
  const fields = kind === 'groups' ? ['name', 'issuance_enabled', 'virtual_plan_name', 'virtual_usage_limit', 'rate_card_id', 'margin_multiplier'] : ['exposed_model_id', 'target_provider_id', 'target_model', 'group_id', 'context_window', 'max_output', 'credit_multiplier', 'visible', 'supports_tools', 'supports_vision', 'supports_reasoning'];
  const labels: Record<string, string> = {name: '分组名称', issuance_enabled: '允许发放新卡', virtual_plan_name: '虚拟套餐名称（非发卡套餐）', virtual_usage_limit: '虚拟用量上限（非发卡积分）', rate_card_id: '价格表 ID', margin_multiplier: '分组扣费倍率（1 = 不加倍）', exposed_model_id: '展示模型 ID', target_provider_id: '供应商 ID', target_model: '上游模型 ID', group_id: '分组 ID', context_window: '上下文长度', max_output: '最大输出', credit_multiplier: '模型扣费倍率（1 = 不加倍）', visible: '发布到用户目录', supports_tools: '工具调用', supports_vision: '视觉', supports_reasoning: '推理'};
  const load = async () => {
    setBusy(true);
    try {
      const result = await adminApi.getCommercialConfig();
      if (!result.success) throw new Error('服务器未确认配置读取成功，请重新读取。');
      setConfig(result.config); setSelected(0); setPriceDraft(null);
      setLoadedDraft(JSON.stringify(kind === 'groups' ? {groups: result.config.groups} : {models: result.config.models, rate_cards: result.config.rate_cards, versions: []}, null, 2));
      setReason('');
      setDraft(JSON.stringify(kind === 'groups' ? {groups: result.config.groups} : {models: result.config.models, rate_cards: result.config.rate_cards, versions: []}, null, 2));
      setMessage('当前配置已加载。历史价格只读；调整价格请创建新版本。');
    } catch (e) { setMessage(String(e)); } finally { setBusy(false); }
  };
  useEffect(() => { void load(); }, [kind]);
  const publish = async () => {
    if (!config || busy) return;
    try {
      if(priceDraft)throw new Error('价格编辑尚未加入发布草稿，请先点击“加入价格草稿”，或取消价格编辑后再发布。');
      const parsed: unknown = JSON.parse(draft);
      if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error('配置必须是 JSON 对象');
      if (!reason.trim()) throw new Error('请填写变更原因');
      if (!window.confirm('确认发布配置？新请求将使用新映射，价格按生效时间启用。有未结算请求时发布会被拒绝。')) return;
      setBusy(true);
      const result = await adminApi.publishCommercialConfig({...parsed, expected_revision: config.revision, reason});
      if (!result.success) throw new Error('服务器未确认发布成功，请重新读取配置核对后再操作。');
      setConfig(result.config); setReason(''); setPriceDraft(null);
      setLoadedDraft(JSON.stringify(kind === 'groups' ? {groups: result.config.groups} : {models: result.config.models, rate_cards: result.config.rate_cards, versions: []}, null, 2));
      setReason('');
      setDraft(JSON.stringify(kind === 'groups' ? {groups: result.config.groups} : {models: result.config.models, rate_cards: result.config.rate_cards, versions: []}, null, 2));
      setMessage('发布成功，配置与审计记录已保存。');
    } catch (e) { setMessage(String(e)); } finally { setBusy(false); }
  };
  return <div className="space-y-6">
    {kind === 'models' && <div className="step-bar"><span>01 选择模型</span><span>02 映射路由</span><span>03 积分价格</span><span>04 校验发布</span></div>}
    <section className="panel"><h3>{kind === 'groups' ? '模型与计费分组' : '模型目录与路由'}</h3><p className="muted">{kind === 'groups' ? '套餐决定积分额度、名称和 30 天有效期；分组决定模型和价格，切换套餐不改变分组。关闭发放仅禁止新卡，不改变已发卡密。' : '模型发现不等于 Key 授权。请核对可用路由和价格后发布。'}</p>
      <table><thead><tr>{(kind === 'groups' ? ['分组', '虚拟套餐（非发卡套餐）', '价格表', '操作'] : ['展示名称', '上游模型 ID', '供应商 / Key 池', '操作']).map(label => <th key={label}>{label}</th>)}</tr></thead><tbody>{rows.map((row, index) => <tr key={String(row.id ?? index)} className={selected === index ? 'selected-row' : ''}><td>{String(row[kind === 'groups' ? 'name' : 'exposed_model_id'] ?? row.id)}</td><td>{String(row[kind === 'groups' ? 'virtual_plan_name' : 'target_model'] ?? '—')}</td><td>{String(row[kind === 'groups' ? 'rate_card_id' : 'target_provider_id'] ?? '—')}</td><td><button onClick={() => setSelected(index)}>编辑配置 →</button></td></tr>)}{!rows.length && <tr><td colSpan={4} className="empty-state">暂无已读取的配置</td></tr>}</tbody></table>
    </section>
    <div className="two-columns"><section className="panel"><h3>正在编辑 · {String(selectedRow?.name ?? selectedRow?.exposed_model_id ?? '请选择条目')}</h3><fieldset disabled={busy || !selectedRow} className="field-grid">{selectedRow && fields.filter(field => field === 'issuance_enabled' || field in selectedRow).map(field => <label key={field}>{labels[field]}{field === 'issuance_enabled' || typeof selectedRow[field] === 'boolean' ? <input type="checkbox" checked={field === 'issuance_enabled' ? selectedRow[field] !== false : Boolean(selectedRow[field])} onChange={e => updateField(field, e.target.checked)} /> : <input type={typeof selectedRow[field] === 'number' ? 'number' : 'text'} step="any" value={String(selectedRow[field] ?? '')} onChange={e => updateField(field, typeof selectedRow[field] === 'number' ? Number(e.target.value) : e.target.value)} />}</label>)}</fieldset></section><section className="panel"><h3>发布前检查</h3><p className="muted">版本：{config?.revision ?? '尚未读取'}</p><p>填写变更原因后发布。发布前会检查版本冲突、路由配置和未结算请求。</p><p className="muted">历史价格不可覆盖，新价格按指定时间生效。修改分组配置不会自动调整已发卡密余额。</p></section></div>
    {kind === 'models' && <section className="panel pricing-editor">
      <div className="pricing-heading"><div><h3>积分价格</h3><p className="muted">客户固定售价 · 自动换算微积分 · 与采购成本独立</p></div><span className="pricing-badge">版本化定价</span></div>
      <div className="pricing-template"><label>选择价格版本模板<select disabled={busy} aria-label="选择价格版本模板" value={String(priceDraft?.source_id ?? '')} onChange={e => {const version = config?.versions.find(v => v.id === e.target.value); if (!version) {setPriceDraft(null); return;} try {const inputs = Object.fromEntries(Object.keys(priceFields).map(field => [field, formatMicroPrice(Number(version[field]), priceUnit)])); setPriceDraft({...version, id: '', effective_from_secs: 0, source_id: version.id}); setPriceInputs(inputs);} catch (error) {setPriceDraft(null); setMessage('模板价格无法安全读取：' + String(error));}}}><option value="">从现有版本创建新草稿</option>{config?.versions.filter(v => v.pricing_mode === 'fixed').map(v => <option key={String(v.id)} value={String(v.id)}>{String(v.model)} · {String(v.id)}</option>)}</select></label><details><summary>价格版本与生效规则</summary><p>历史版本只读。新版本沿用模板的模型与价格表，按指定时间生效。1 积分 = 1,000,000 微积分。草稿仅保留在当前页面；成本加成、按次计费及首个价格版本使用高级配置。</p></details></div>
      {priceDraft && <fieldset disabled={busy} className="pricing-form">
        <div className="pricing-grid pricing-meta">
          <label>新版本 ID<input value={String(priceDraft.id)} onChange={e => setPriceDraft({...priceDraft, id: e.target.value})}/></label>
          <label>生效时间（本地时区）<input type="datetime-local" onChange={e => setPriceDraft({...priceDraft, effective_from_secs: Math.floor(new Date(e.target.value).getTime()/1000)})}/></label>
          <label>客户售价单位<select value={priceUnit} onChange={e => changePriceUnit(e.target.value as PriceUnit)}><option value="million">积分 / 百万 Tokens</option><option value="thousand">积分 / 千 Tokens</option></select></label>
          <label>价格版本扣费倍率（1 = 不加倍）<input inputMode="decimal" value={String(priceDraft.margin_multiplier ?? '')} onChange={e => setPriceDraft({...priceDraft, margin_multiplier: e.target.value})}/></label>
        </div>
        <div className="pricing-grid pricing-rates">{Object.entries(priceFields).map(([field, label]) => <label key={field}>{label}售价（积分 / {priceUnit === 'million' ? '百万' : '千'} Tokens）<input inputMode="decimal" value={priceInputs[field] ?? ''} onChange={e => setPriceInputs({...priceInputs, [field]: e.target.value})}/><small>{convertedPrice(field)}</small></label>)}</div>
        <section className="pricing-preview" aria-label="客户扣费预览">
          <div className="pricing-heading"><div><h4>客户扣费预览（当前草稿）</h4><p className="muted">模型：{String(selectedRow?.exposed_model_id ?? '未选择')} · 分组：{String(previewGroup?.name ?? previewGroup?.id ?? '未找到')}</p></div><span className="pricing-badge">模拟用量</span></div>
          <div className="pricing-grid pricing-tokens">{['未缓存输入', '输出', '缓存写入', '缓存读取'].map((label, index) => <label key={label}>{label} Tokens<input inputMode="numeric" value={tokenInputs[index]} onChange={e => setTokenInputs(values => values.map((value, i) => i === index ? e.target.value : value))}/></label>)}</div>
          <p className="pricing-formula">用量费用合计 × 版本 {String(priceDraft.margin_multiplier ?? '未填写')} × 分组 {String(previewGroup?.margin_multiplier ?? '未找到')} × 模型 {String(selectedRow?.credit_multiplier ?? '未找到')} · 最终向上取整至 1 微积分</p>
          <p role="status" className={'pricing-result' + (previewError ? ' pricing-result-error' : '')}>{previewError || <>预计扣费：<strong>{preview}</strong></>}</p>
          <p className="pricing-footnote">未缓存输入不含缓存用量。仅模拟草稿，实际扣费以生效版本和结算账本为准；不是采购成本或人民币收入。</p>
        </section>
        <section className="pricing-procurement" aria-label="采购参考价格">
          <div className="pricing-heading"><div><h4>采购参考价格</h4><p className="muted">采购价格用于成本估算，不参与固定模式客户扣费。仅明确免费时填 0。</p></div><label>采购计价币种<select value={String(priceDraft.currency??'')} onChange={e=>setPriceDraft({...priceDraft,currency:e.target.value})}><option value="">请选择</option><option value="USD">USD</option><option value="CNY">CNY</option></select></label></div>
          <div className="pricing-grid">{Object.entries(costFields).map(([field,label])=><label key={field}>{label}（计价货币 / 百万 Tokens）<input type="number" min="0" step="any" value={String(priceDraft[field]??'')} onChange={e=>setPriceDraft({...priceDraft,[field]:e.target.value})}/></label>)}</div>
        </section>
        <div className="pricing-actions"><button className="primary" onClick={stagePrice}>加入价格草稿</button><button onClick={() => {if(window.confirm('确认丢弃尚未加入发布草稿的价格编辑？')){setPriceDraft(null);setPriceInputs({});}}}>取消价格编辑</button><span className="muted">加入草稿后，仍需填写原因并确认发布。</span></div>
      </fieldset>}
    </section>}
    <section className="p-6 rounded-xl bg-white border border-[#E5E8E5] space-y-4">
    <h3 className="font-semibold text-[#23272B]">{kind === 'groups' ? '分组配置' : '模型映射与版本化定价'}</h3>
    <p className="text-[#7B8388] text-sm">修改后填写原因并发布；离开页面会丢弃未发布草稿。新增条目及完整参数可在高级配置中编辑。</p>
    <p role="status" className="text-[#A87029] text-sm">{message}</p>
    <button disabled={busy} onClick={() => { if (!config || window.confirm('重新读取将丢弃未发布的编辑，继续吗？')) void load(); }} className="px-4 py-2 bg-[#EFF1EF] rounded">重新读取配置</button>
    <details><summary>高级配置 JSON · 新增条目与价格版本</summary><label className="block">配置 JSON<textarea aria-label="配置 JSON" value={draft} disabled={busy} onChange={e => setDraft(e.target.value)} spellCheck={false} className="block w-full h-96 bg-white font-mono text-sm p-3 border border-[#E5E8E5] rounded" /></label></details>
    <label className="block">变更原因<input aria-label="变更原因" value={reason} maxLength={500} disabled={busy} onChange={e=>setReason(e.target.value)} className="block w-full bg-white p-3 border border-[#E5E8E5] rounded" /></label>
    <button disabled={busy || !config || !reason.trim()} onClick={()=>void publish()} className="px-4 py-2 rounded bg-[#B94B39] text-white disabled:opacity-50">{busy ? '处理中…' : '确认并发布'}</button>
    {kind === 'models' && <section className="panel" aria-label="历史价格版本">
      <h3>价格版本列表（只读）</h3>
      <p className="muted">包括历史及已发布的未来版本，不代表全部正在生效。客户基准售价已从微积分自动换算为积分；实际扣费还需叠乘价格版本、分组和模型倍率。时间按本地时区显示。</p>
      <div className="overflow-auto"><table><thead><tr><th>版本 / 模型 / 价格表</th><th>客户基准售价（倍率前）</th><th>价格版本倍率</th><th>采购参考价（与客户售价分开）</th><th>生效时间（本地时区）</th></tr></thead><tbody>
        {config?.versions.map(version => <tr key={String(version.id)}>
          <td>{String(version.id)}<br/>{String(version.model)}<br/>{String(version.rate_card_id)}</td>
          <td>{version.pricing_mode === 'fixed' ? <><strong>固定积分 · 积分 / 百万 Tokens</strong>{Object.entries(priceFields).map(([field, label]) => <div key={field}>{label}：{historyCredits(version[field])}</div>)}</>
            : version.pricing_mode === 'per_call' ? <>按次计费：{historyCredits(version.per_call_credit)} 积分 / 次</>
            : version.pricing_mode === 'cost_plus' ? <>成本加成（非固定积分售价）：按用量、配置采购价、汇率、积分面值及三个倍率计算；无固定积分单价。</>
            : <>未知计费模式：{String(version.pricing_mode ?? '未提供')}</>}</td>
          <td>{typeof version.margin_multiplier === 'number' && Number.isFinite(version.margin_multiplier) && version.margin_multiplier >= 0 ? String(version.margin_multiplier) : '未提供有效倍率'}</td>
          <td>{['USD', 'CNY'].includes(String(version.currency)) ? <>{String(version.currency)} / 百万 Tokens{Object.entries(costFields).map(([field, label]) => <div key={field}>{label}：{typeof version[field] === 'number' && Number.isFinite(version[field]) && Number(version[field]) >= 0 ? String(version[field]) : '未提供有效价格'}</div>)}</> : '未提供有效采购币种'}</td>
          <td>{historyTime(version.effective_from_secs)}</td>
        </tr>)}
        {!config?.versions.length && <tr><td colSpan={5}>暂无已读取的价格版本</td></tr>}
      </tbody></table></div>
    </section>}
    <details><summary>原始配置与历史版本 JSON（只读排障）</summary><pre className="overflow-auto text-xs p-3">{JSON.stringify(config,null,2)}</pre></details>
  </section></div>;
}
