import { parseTokenInput, formatTokens } from './tokens';
import { priceToMicroPerMillion, formatMicroPrice, previewFixedCharge, type PriceUnit } from './pricing';
import { useEffect, useRef, useState } from 'react';
import { adminApi, AdminApiError, CommercialConfig } from './api';

export default function CommercialEditor({ kind,onDirtyChange,onBusyChange }: {kind: 'groups' | 'models';onDirtyChange:(dirty:boolean)=>void;onBusyChange:(busy:boolean)=>void}) {
  const [config, setConfig] = useState<CommercialConfig | null>(null);
  const [draft, setDraft] = useState(''),[loadedDraft,setLoadedDraft]=useState('');
  const [reason, setReason] = useState('');
  const [message, setMessage] = useState('');
  const [busy, setBusy] = useState(false);
  useEffect(()=>{onBusyChange(busy);return()=>onBusyChange(false);},[busy,onBusyChange]);
  const [selected, setSelected] = useState<string | null>(null);
  const [priceDraft, setPriceDraft] = useState<Record<string, unknown> | null>(null);
  const [priceInputs, setPriceInputs] = useState<Record<string, string>>({});
  useEffect(()=>{onDirtyChange(draft!==loadedDraft||!!reason.trim()||!!priceDraft);},[draft,loadedDraft,reason,priceDraft,onDirtyChange]);
  const [priceUnit, setPriceUnit] = useState<PriceUnit>('million');
  const [tokenInputs, setTokenInputs] = useState(['1000', '1000', '0', '0']);
  const pending = useRef(false), alive = useRef(true);
  const [needsReview, setNeedsReview] = useState(false);
  const dirty = draft !== loadedDraft || !!reason.trim() || !!priceDraft;
  const validText = (value: unknown, max: number) => typeof value === 'string' && !!value.trim() && new TextEncoder().encode(value).length <= max && !/[\x00-\x1f\x7f-\x9f]/.test(value);
  const positive = (value: unknown) => typeof value === 'number' && Number.isFinite(value) && value > 0 && value <= 1000;
  const costFields={input_price_per_m:'采购输入价格',output_price_per_m:'采购输出价格',cache_read_price_per_m:'采购缓存读取价格',cache_creation_price_per_m:'采购缓存写入价格'};
  const priceFields = {fixed_input_credit_per_m: '未缓存输入', fixed_output_credit_per_m: '输出', fixed_cache_read_credit_per_m: '缓存读取', fixed_cache_creation_credit_per_m: '缓存写入'};
  const stagePrice = () => {
    try {
      if (draftError) throw new Error('高级配置 JSON 无效，请先修正；价格编辑已保留');
      if (!priceDraft || !validText(priceDraft.id, 128)) throw new Error('请填写新的价格版本 ID，不超过 128 字节且不能包含控制字符');
      if (config?.versions.some(v => v.id === priceDraft.id)) throw new Error('版本 ID 已存在，不能覆盖历史价格');
      if (Array.isArray(parsedDraft.versions) && parsedDraft.versions.some(v => v.id === priceDraft.id)) throw new Error('版本 ID 已在本页草稿中，请使用新的 ID，避免覆盖已加入的价格');
      if (!Number.isSafeInteger(priceDraft.effective_from_secs) || Number(priceDraft.effective_from_secs) <= Date.now() / 1000) throw new Error('生效时间须晚于当前时间，不能追溯生效；请预留发布操作时间');
      if(!['USD','CNY'].includes(String(priceDraft.currency)))throw new Error('请选择采购计价币种');
      if (!positive(Number(priceDraft.margin_multiplier))) throw new Error('价格版本倍率必须为非负有限数，且须大于 0、至多 1000');
      const costs=Object.fromEntries(Object.keys(costFields).map(field=>{const raw=String(priceDraft[field]??'');const value=Number(raw);if(!raw.trim()||!Number.isFinite(value)||value<0||value>1_000_000)throw new Error('四类采购价格须为 0 至 1,000,000 的有限数，明确免费时才填 0');return [field,value];}));
      const values = Object.fromEntries(Object.keys(priceFields).map(field => [field, priceToMicroPerMillion(priceInputs[field] ?? '', priceUnit)]));
      if (Object.values(values).some(value => value > 1_000_000_000_000_000)) throw new Error('客户售价超过服务端允许范围：最多 1,000,000,000 积分 / 百万 Tokens');
      const versions = [...(Array.isArray(parsedDraft.versions) ? parsedDraft.versions : []), Object.fromEntries(Object.entries({...priceDraft, margin_multiplier: Number(priceDraft.margin_multiplier), ...values, ...costs}).filter(([key]) => key !== 'source_id'))];
      setDraft(JSON.stringify({...parsedDraft, versions}, null, 2)); setPriceDraft(null); setPriceInputs({}); setMessage('价格版本已加入本页草稿，尚未发布。请填写原因并确认发布。');
    } catch (error) { setMessage(String(error)); }
  };
  let parsedDraft: Record<string, Array<Record<string, unknown>>> = {};
  let draftError = '';
  try {
    const value = JSON.parse(draft);
    if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('配置必须是 JSON 对象');
    for (const key of Object.keys(value)) {
      if (!['groups', 'models', 'rate_cards', 'versions', 'settings'].includes(key)) throw new Error(`不支持的配置字段：${key}`);
      if (key !== 'settings' && (!Array.isArray(value[key]) || value[key].some((row: unknown) => !row || typeof row !== 'object' || Array.isArray(row)))) throw new Error(`${key} 必须是对象数组`);
    }
    parsedDraft = value;
  } catch (error) { draftError = error instanceof SyntaxError ? '配置 JSON 格式无效，请检查逗号、引号和括号。' : String(error); }
  const rows = Array.isArray(parsedDraft[kind]) ? parsedDraft[kind].filter(row => row && typeof row === 'object' && !Array.isArray(row)) : [];
  const selectedRow = selected === null ? rows[0] : rows.find(row => row.id === selected);
  const editorRef = useRef<HTMLElement>(null);
  const originalRows = config?.[kind] ?? [];
  const isEdited = (row: Record<string, unknown>) => JSON.stringify(row) !== JSON.stringify(originalRows.find(original => original.id === row.id));
  const focusEditor = () => { editorRef.current?.scrollIntoView({block: 'start'}); editorRef.current?.focus({preventScroll: true}); };
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
  const priceTime = Number(priceDraft?.effective_from_secs);
  const priceDate = new Date(priceTime * 1000);
  const localPriceTime = priceTime > 0 && Number.isFinite(priceDate.getTime()) ? new Date(priceDate.getTime() - priceDate.getTimezoneOffset() * 60000).toISOString().slice(0, 16) : '';
  const changePriceUnit = (unit: PriceUnit) => {
    try {
      const next = Object.fromEntries(Object.entries(priceInputs).map(([field, value]) => [field, formatMicroPrice(priceToMicroPerMillion(value, priceUnit), unit)]));
      setPriceInputs(next); setPriceUnit(unit);
    } catch (error) { setMessage('切换单位前请修正售价：' + String(error)); }
  };
  const updateField = (field: string, value: unknown) => {
    if (!selectedRow || rows.filter(row => row.id === selectedRow.id).length !== 1) {setMessage('当前条目 ID 重复或不存在，请先修正高级配置；未修改任何条目。'); return;}
    const next = rows.map(row => row.id === selectedRow?.id ? {...row, [field]: value} : row);
    setDraft(JSON.stringify({...parsedDraft, [kind]: next}, null, 2));
  };
  const numericFields = ['virtual_usage_limit', 'margin_multiplier', 'context_window', 'max_output', 'credit_multiplier', 'rate_multiplier'];
  // Shown in the customer's model list; left empty, the server derives them.
  const optionalFields = ['display_name', 'description', 'rate_multiplier'];
  const fields = kind === 'groups' ? ['name', 'issuance_enabled', 'virtual_plan_name', 'virtual_usage_limit', 'rate_card_id', 'margin_multiplier'] : ['exposed_model_id', 'display_name', 'description', 'rate_multiplier', 'target_provider_id', 'target_model', 'group_id', 'context_window', 'max_output', 'credit_multiplier', 'visible', 'supports_tools', 'supports_vision', 'supports_reasoning'];
  const labels: Record<string, string> = {name: '分组名称', issuance_enabled: '允许发放新卡', virtual_plan_name: '虚拟套餐名称（非发卡套餐）', virtual_usage_limit: '虚拟用量上限（非发卡积分）', rate_card_id: '价格表 ID', margin_multiplier: '分组扣费倍率（1 = 不加倍）', exposed_model_id: '展示模型 ID', target_provider_id: '供应商 ID', target_model: '上游模型 ID', group_id: '分组 ID', context_window: '上下文长度', max_output: '最大输出', credit_multiplier: '模型扣费倍率（1 = 不加倍）', display_name: '用户侧显示名称（留空按模型 ID 生成）', description: '用户侧模型说明（留空为“名称 model”）', rate_multiplier: '用户侧显示倍率（如 1.3，显示为 1.3x Credit；留空按价格换算；仅展示，不影响扣费）', visible: '发布到用户目录', supports_tools: '工具调用', supports_vision: '视觉', supports_reasoning: '推理'};
  // An unpublished draft survives a session end, restored only onto the configuration it
  // was made from; the draft holds no secret.
  const draftKey = `admin-commercial-draft:v1:${kind}`;
  const apply = (next: CommercialConfig): 'restored' | 'stale' | 'none' => {
    const value = JSON.stringify(kind === 'groups' ? {groups: next.groups} : {models: next.models, rate_cards: next.rate_cards, versions: []}, null, 2);
    let saved: {base?: unknown; draft?: unknown; reason?: unknown} = {};
    try {saved = JSON.parse(sessionStorage.getItem(draftKey) || '{}') ?? {};} catch {/* nothing to restore */}
    const restore = saved.base === value && typeof saved.draft === 'string' && typeof saved.reason === 'string';
    setConfig(next); setSelected(String(next[kind][0]?.id ?? '')); setPriceDraft(null); setPriceInputs({}); setReason(restore ? String(saved.reason) : ''); setLoadedDraft(value); setDraft(restore ? String(saved.draft) : value); setNeedsReview(false);
    return restore ? 'restored' : typeof saved.draft === 'string' ? 'stale' : 'none';
  };
  useEffect(() => {
    if (!loadedDraft) return;
    try {
      if (draft !== loadedDraft || reason.trim()) sessionStorage.setItem(draftKey, JSON.stringify({base: loadedDraft, draft, reason}));
      else sessionStorage.removeItem(draftKey);
    } catch {/* A draft that cannot be kept is only lost at a session end. */}
  }, [draft, loadedDraft, reason, draftKey]);
  const load = async () => {
    if (pending.current) return;
    pending.current = true; setBusy(true); setMessage('正在读取配置…');
    try {
      const result = await adminApi.getCommercialConfig();
      if (result.success !== true || !result.config?.revision) throw new Error('服务器未确认配置读取成功');
      if (alive.current) {
        const draftState = apply(result.config);
        setMessage(draftState === 'restored' ? '已恢复会话到期前未发布的草稿，请核对后发布。' : draftState === 'stale' ? '配置已在别处更新，之前未发布的草稿基于旧配置，未恢复；请在当前配置上重新编辑。' : '当前配置已加载。历史价格只读；调整价格请创建新版本。');
      }
    } catch (error) {
      if (alive.current) {setNeedsReview(true); setMessage(`${error instanceof Error ? error.message : String(error)}。读取失败，原草稿已保留；重新读取成功前不可发布。`);}
    } finally {pending.current = false; if (alive.current) setBusy(false);}
  };
  useEffect(() => { alive.current = true; void load(); return () => {alive.current = false;}; }, [kind]);
  const publish = async () => {
    if (!config || pending.current || needsReview) return;
    let submitted = false;
    try {
      if (priceDraft) throw new Error('价格编辑尚未加入发布草稿，请先点击“加入价格草稿”，或取消价格编辑后再发布。');
      if (draftError) throw new Error(draftError);
      if (!validText(reason.trim(), 500)) throw new Error('请填写变更原因，不超过 500 字节且不能包含控制字符（中文通常占 3 字节）');
      for (const [section, entries] of Object.entries(parsedDraft)) {
        if (section === 'settings') continue;
        const ids = new Set();
        for (const row of entries) {
          if (!validText(row.id, 128) || ids.has(row.id)) throw new Error(`${section} 中的 ID 不能为空、重复或超过 128 字节`);
          ids.add(row.id);
          if (section === 'groups') {
            if (!validText(row.name, 256) || ('virtual_plan_name' in row && !validText(row.virtual_plan_name, 256))) throw new Error('分组名称和虚拟套餐名称不能为空，且不得超过 256 字节');
            if ('margin_multiplier' in row && !positive(row.margin_multiplier)) throw new Error('分组扣费倍率须大于 0、至多 1000，空白不能作为 0');
            if ('virtual_usage_limit' in row && (typeof row.virtual_usage_limit !== 'number' || !Number.isFinite(row.virtual_usage_limit) || row.virtual_usage_limit < 0)) throw new Error('虚拟用量上限须为非负有限数，不能留空');
          }
          if (section === 'models') {
            if (!validText(row.exposed_model_id, 128) || !validText(row.target_model, 256) || !validText(row.target_provider_id, 256) || !validText(row.group_id, 128)) throw new Error('请填写有效的展示模型、上游模型、供应商和分组 ID');
            if (!positive(row.credit_multiplier)) throw new Error('模型扣费倍率须大于 0、至多 1000，空白不能作为 0');
            if (!Number.isSafeInteger(row.context_window) || Number(row.context_window) < 1 || Number(row.context_window) > 10_000_000 || !Number.isSafeInteger(row.max_output) || Number(row.max_output) < 1 || Number(row.max_output) > Number(row.context_window)) throw new Error('上下文长度须为 1 至 10,000,000 的整数；最大输出须为正整数且不能超过上下文长度');
            const old = config.models.find(model => model.id === row.id);
            if (old && old.group_id !== row.group_id) throw new Error('已有模型映射不能迁移分组；请在高级配置中使用新的映射 ID 新增条目');
          }
          if (section === 'versions') {
            if (config.versions.some(version => version.id === row.id)) throw new Error('版本 ID 已存在，不能覆盖历史价格');
            if (!Number.isSafeInteger(row.effective_from_secs) || Number(row.effective_from_secs) <= Date.now() / 1000) throw new Error('价格生效时间须晚于当前时间，请在高级配置中修正草稿时间');
            if (!positive(row.margin_multiplier)) throw new Error('价格版本倍率须大于 0、至多 1000');
          }
        }
      }
      if (!window.confirm(`确认发布${kind === 'groups' ? '分组' : '模型与价格'}配置？\n本页修改将提交到版本 ${config.revision}。新请求使用新映射；${parsedDraft.versions?.length ?? 0} 个新价格版本按指定时间生效。\n有未结算请求或版本冲突时服务端将拒绝。原因：${reason.trim()}`)) return;
      pending.current = true; submitted = true; setBusy(true); setMessage('正在发布配置，请勿重复提交…');
      const result = await adminApi.publishCommercialConfig({...parsedDraft, expected_revision: config.revision, reason: reason.trim()});
      if (result.success !== true) throw new AdminApiError('服务器未确认发布成功', 400);
      if (!result.config?.revision) throw new Error('服务器未返回可核对的配置版本');
      try {sessionStorage.removeItem(draftKey);} catch {/* published; nothing left to keep */}
      if (alive.current) {apply(result.config); setMessage('发布成功，配置与审计记录已保存。');}
    } catch (error) {
      if (alive.current) {
        const mustReview = submitted && !(error instanceof AdminApiError && [400, 401, 403, 413, 422].includes(error.status));
        if (mustReview) setNeedsReview(true);
        setMessage(`${error instanceof Error ? error.message : String(error)}${mustReview ? '。草稿已保留，发布已暂停。请重新读取配置核对结果或版本冲突，不要重复提交。' : ''}`);
      }
    } finally { if (submitted) {pending.current = false; if (alive.current) setBusy(false);} }
  };
  const selectTemplate = (id: string) => {
    if (priceDraft && !window.confirm('切换模板将丢弃尚未加入发布草稿的价格编辑，继续吗？')) return;
    const version = config?.versions.find(value => value.id === id);
    if (!version) {setPriceDraft(null); setPriceInputs({}); return;}
    try {
      const inputs = Object.fromEntries(Object.keys(priceFields).map(field => [field, formatMicroPrice(Number(version[field]), priceUnit)]));
      setPriceDraft({...version, id: '', effective_from_secs: 0, source_id: version.id}); setPriceInputs(inputs);
    } catch (error) {setMessage('模板价格无法安全读取，原编辑已保留：' + String(error));}
  };
  return <div className="space-y-6">
    {kind === 'models' && <div className="step-bar"><span>01 选择模型</span><span>02 映射路由</span><span>03 积分价格</span><span>04 校验发布</span></div>}
    <section className="panel"><h3>{kind === 'groups' ? '模型与计费分组' : '模型目录与路由'}</h3><p className="muted">{kind === 'groups' ? '分组决定模型和价格；关闭发放仅禁止新卡，不影响已发卡密。' : '模型发现不等于 Key 授权。请核对可用路由和价格后发布。'}</p>
      <div className="editor-list"><table><thead><tr>{(kind === 'groups' ? ['分组', '虚拟套餐（非发卡套餐）', '价格表', '操作'] : ['展示名称', '上游模型 ID', '供应商 / Key 池', '操作']).map(label => <th key={label}>{label}</th>)}</tr></thead><tbody>{rows.map((row, index) => <tr key={String(row.id ?? index)} className={selectedRow?.id === row.id ? 'selected-row' : ''}><td>{String(row[kind === 'groups' ? 'name' : 'exposed_model_id'] ?? row.id)}</td><td>{String(row[kind === 'groups' ? 'virtual_plan_name' : 'target_model'] ?? '—')}</td><td>{String(row[kind === 'groups' ? 'rate_card_id' : 'target_provider_id'] ?? '—')}</td><td><button disabled={busy} onClick={() => {setSelected(String(row.id)); focusEditor();}}>编辑配置 →</button>{isEdited(row) && <span className="edited-badge">已修改</span>}</td></tr>)}{!rows.length && <tr><td colSpan={4} className="empty-state">暂无已读取的配置</td></tr>}</tbody></table></div>
      <div className="editor-jump"><label>定位配置<select aria-label="定位配置" disabled={busy} value={String(selectedRow?.id ?? '')} onChange={e => {setSelected(e.target.value); focusEditor();}}><option value="" disabled>请选择条目</option>{rows.map(row => <option key={String(row.id)} value={String(row.id)}>{isEdited(row) ? '[已修改] ' : ''}{String(row.name ?? row.exposed_model_id ?? row.id)}</option>)}</select></label><span className="muted">{rows.filter(isEdited).length} 项已修改 · 尚未发布</span></div>
    </section>
    <div className="compact-editor"><section ref={editorRef} tabIndex={-1} className="panel mapping-editor"><h3>正在编辑 · {String(selectedRow?.name ?? selectedRow?.exposed_model_id ?? '请选择条目')}</h3><fieldset disabled={busy || !selectedRow} className="field-grid">{selectedRow && fields.filter(field => field === 'issuance_enabled' || field in selectedRow).map(field => <label key={field}>{labels[field]}{['context_window', 'max_output'].includes(field) && '（Tokens）'}{field === 'issuance_enabled' || typeof selectedRow[field] === 'boolean' ? <input type="checkbox" checked={field === 'issuance_enabled' ? selectedRow[field] !== false : Boolean(selectedRow[field])} onChange={e => updateField(field, e.target.checked)} /> : <input aria-label={labels[field]} placeholder={['context_window', 'max_output'].includes(field) ? '整数或 K / M，例如 128K' : undefined} type={numericFields.includes(field) && !['context_window', 'max_output'].includes(field) ? 'number' : 'text'} step={['context_window', 'max_output'].includes(field) ? '1' : 'any'} value={String(selectedRow[field] ?? '')} onChange={e => updateField(field, optionalFields.includes(field) && !e.target.value.trim() ? null : ['context_window', 'max_output'].includes(field) ? parseTokenInput(e.target.value) : numericFields.includes(field) && e.target.value.trim() ? Number(e.target.value) : e.target.value)} />}{['context_window', 'max_output'].includes(field) && <small>{formatTokens(selectedRow[field])} · 1K = 1,000；1M = 1,000,000</small>}</label>)}</fieldset></section><details className="editor-checks"><summary>发布前检查 · 版本 {config?.revision ?? '尚未读取'}</summary><p className="muted">版本：{config?.revision ?? '尚未读取'}</p><p>填写变更原因后发布。发布前会检查版本冲突、路由配置和未结算请求。</p><p className="muted">{kind === 'groups' ? '分组倍率须大于 0、至多 1000；1 表示不加倍。虚拟用量上限不是卡密余额；修改分组不会给已发卡密充值。' : '模型与价格版本倍率须大于 0、至多 1000；1 表示不加倍。最大输出不得超过上下文长度。已有映射不能迁移分组；发布到用户目录前须有启用且授权兼容的 Key。'}</p></details></div>
    {kind === 'models' && <section className="panel pricing-editor">
      <div className="pricing-heading"><div><h3>积分价格</h3><p className="muted">客户固定售价 · 自动换算微积分 · 与采购成本独立</p></div><span className="pricing-badge">版本化定价</span></div>
      <div className="pricing-template"><label>选择价格版本模板<select disabled={busy} aria-label="选择价格版本模板" value={String(priceDraft?.source_id ?? '')} onChange={e => selectTemplate(e.target.value)}><option value="">从现有版本创建新草稿</option>{config?.versions.filter(v => v.pricing_mode === 'fixed').map(v => <option key={String(v.id)} value={String(v.id)}>{String(v.model)} · {String(v.id)}</option>)}</select></label><details><summary>价格版本与生效规则</summary><p>历史版本只读。新版本沿用模板的模型与价格表，只能在未来时间生效，请预留发布操作时间。1 积分 = 1,000,000 微积分。草稿仅保留在当前页面；成本加成、按次计费及首个价格版本使用高级配置。</p></details></div>
      {priceDraft && <fieldset disabled={busy} className="pricing-form">
        <div className="pricing-grid pricing-meta">
          <label>新版本 ID<input value={String(priceDraft.id)} onChange={e => setPriceDraft({...priceDraft, id: e.target.value})}/></label>
          <label>生效时间（本地时区）<input type="datetime-local" value={localPriceTime} onChange={e => setPriceDraft({...priceDraft, effective_from_secs: Math.floor(new Date(e.target.value).getTime()/1000)})}/></label>
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
    <details><summary>发布与草稿规则</summary><p className="text-[#7B8388] text-sm">修改后填写原因并发布；离开页面会丢弃未发布草稿。发布会按 ID 新增或更新条目，从 JSON 删除一行不等于删除服务端条目。新增条目及完整参数可在高级配置中编辑。</p></details>
    <p role="status" className="text-[#A87029] text-sm">{message}</p>
    {config && draftError && <p role="alert">{draftError} 原输入已保留，修正后才能加入价格或发布。</p>}
    <p className="muted">{dirty ? `有未发布的编辑 · 已加入 ${Array.isArray(parsedDraft.versions) ? parsedDraft.versions.length : 0} 个价格版本` : '当前没有未发布的编辑'}{needsReview ? ' · 请重新读取后核对，当前禁止发布' : ''}</p>
    <button disabled={busy} onClick={() => { if (!dirty || window.confirm('重新读取将丢弃未发布的编辑，继续吗？')) void load(); }} className="px-4 py-2 bg-[#EFF1EF] rounded">重新读取配置</button>
    <details><summary>高级配置 JSON · 新增条目与价格版本</summary><label className="block">配置 JSON<textarea aria-label="配置 JSON" value={draft} disabled={busy} onChange={e => setDraft(e.target.value)} spellCheck={false} className="block w-full h-96 bg-white font-mono text-sm p-3 border border-[#E5E8E5] rounded" /></label></details>
    <label className="block">变更原因<input aria-label="变更原因" value={reason} maxLength={500} disabled={busy} onChange={e=>setReason(e.target.value)} className="block w-full bg-white p-3 border border-[#E5E8E5] rounded" /></label>
    <p className="muted">变更原因用于审计，最多 500 字节（中文通常占 3 字节）。重新读取成功后会丢弃本页草稿，请先保留需要的内容。</p>
    <button disabled={busy || needsReview || !config || !reason.trim()} onClick={()=>void publish()} className="px-4 py-2 rounded bg-[#B94B39] text-white disabled:opacity-50">{busy ? '处理中…' : '确认并发布'}</button>
    {kind === 'models' && <section className="panel" aria-label="历史价格版本">
      <h3>价格版本列表（只读）</h3>
      <details><summary>价格与倍率说明</summary><p className="muted">包括历史及已发布的未来版本，不代表全部正在生效。客户基准售价已从微积分自动换算为积分；实际扣费还需叠乘价格版本、分组和模型倍率。时间按本地时区显示。</p></details>
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
