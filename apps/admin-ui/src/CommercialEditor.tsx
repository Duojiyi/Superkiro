import { parseTokenInput, formatTokens } from './tokens';
import { priceToMicroPerMillion, formatMicroPrice, previewFixedCharge, type PriceUnit } from './pricing';
import { useEffect, useRef, useState } from 'react';
import { adminApi, AdminApiError, type AdminCardItem, type CommercialConfig } from './api';
import { confirmAction } from './components/confirm';
import { toast } from './components/toast';
import { InfoTip, StatusBadge } from './components/ui';
import { formatCount, formatFullDateTime, formatTokenCount, shortHash } from './format';
import { priceVersionView } from './status';

// 分组与权益 / 模型与定价: a list, the row being edited, and a bar at the bottom with the
// change reason and 发布. Everything is published together against the version read, with
// a reason; a publish without a confirmed result blocks the next one until a reload.
export default function CommercialEditor({ kind, onDirtyChange, onBusyChange, cards, onPublished, refreshEpoch = 0 }: {kind: 'groups' | 'models'; onDirtyChange: (dirty: boolean) => void; onBusyChange: (busy: boolean) => void; cards?: AdminCardItem[]; onPublished?: () => void; /** Changes when the operator asks the whole console to refresh. */ refreshEpoch?: number}) {
  const [config, setConfig] = useState<CommercialConfig | null>(null);
  const [draft, setDraft] = useState(''), [loadedDraft, setLoadedDraft] = useState('');
  const [reason, setReason] = useState('');
  const [message, setMessage] = useState('');
  const [busy, setBusy] = useState(false);
  useEffect(() => {onBusyChange(busy); return () => onBusyChange(false);}, [busy, onBusyChange]);
  const [selected, setSelected] = useState<string | null>(null);
  const [priceDraft, setPriceDraft] = useState<Record<string, unknown> | null>(null);
  const [priceInputs, setPriceInputs] = useState<Record<string, string>>({});
  useEffect(() => {onDirtyChange(draft !== loadedDraft || !!reason.trim() || !!priceDraft);}, [draft, loadedDraft, reason, priceDraft, onDirtyChange]);
  const [priceUnit, setPriceUnit] = useState<PriceUnit>('million');
  const [tokenInputs, setTokenInputs] = useState(['1000', '1000', '0', '0']);
  const pending = useRef(false), alive = useRef(true);
  const [needsReview, setNeedsReview] = useState(false);
  const [messageTone, setMessageTone] = useState<'error' | 'warning' | 'info'>('error');
  const [publishing, setPublishing] = useState(false);
  // The console was refreshed while this page had unpublished edits.
  const [serverChanged, setServerChanged] = useState(false);
  const say = (text: string, tone: 'error' | 'warning' | 'info' = 'error') => {setMessage(text); setMessageTone(tone);};
  const dirty = draft !== loadedDraft || !!reason.trim() || !!priceDraft;
  const validText = (value: unknown, max: number) => typeof value === 'string' && !!value.trim() && new TextEncoder().encode(value).length <= max && !/[\x00-\x1f\x7f-\x9f]/.test(value);
  const positive = (value: unknown) => typeof value === 'number' && Number.isFinite(value) && value > 0 && value <= 1000;
  const costFields = {input_price_per_m: '采购输入价格', output_price_per_m: '采购输出价格', cache_read_price_per_m: '采购缓存读取价格', cache_creation_price_per_m: '采购缓存写入价格'};
  const priceFields = {fixed_input_credit_per_m: '未缓存输入', fixed_output_credit_per_m: '输出', fixed_cache_read_credit_per_m: '缓存读取', fixed_cache_creation_credit_per_m: '缓存写入'};
  const stagePrice = () => {
    try {
      if (draftError) throw new Error('高级配置 JSON 无效，请先修正；价格编辑已保留');
      if (!priceDraft || !validText(priceDraft.id, 128)) throw new Error('版本 ID 无效：不超过 128 字节，不含控制字符');
      if (config?.versions.some(v => v.id === priceDraft.id)) throw new Error('版本 ID 已存在，请换一个，不能覆盖历史价格');
      if (Array.isArray(parsedDraft.versions) && parsedDraft.versions.some(v => v.id === priceDraft.id)) throw new Error('版本 ID 已在本页草稿中，请换一个');
      if (!Number.isSafeInteger(priceDraft.effective_from_secs) || Number(priceDraft.effective_from_secs) <= Date.now() / 1000) throw new Error('生效时间需晚于现在，不能追溯生效');
      if (!['USD', 'CNY'].includes(String(priceDraft.currency))) throw new Error('请选择采购计价币种');
      if (!positive(Number(priceDraft.margin_multiplier))) throw new Error('版本倍率需大于 0、不超过 1000');
      const costs = Object.fromEntries(Object.keys(costFields).map(field => {const raw = String(priceDraft[field] ?? ''); const value = Number(raw); if (!raw.trim() || !Number.isFinite(value) || value < 0 || value > 1_000_000) throw new Error('采购价需在 0–1,000,000 之间（免费填 0）'); return [field, value];}));
      const values = Object.fromEntries(Object.keys(priceFields).map(field => [field, priceToMicroPerMillion(priceInputs[field] ?? '', priceUnit)]));
      if (Object.values(values).some(value => value > 1_000_000_000_000_000)) throw new Error('客户售价超过服务端允许范围：最多 1,000,000,000 积分 / 百万 Tokens');
      const versions = [...(Array.isArray(parsedDraft.versions) ? parsedDraft.versions : []), Object.fromEntries(Object.entries({...priceDraft, margin_multiplier: Number(priceDraft.margin_multiplier), ...values, ...costs}).filter(([key]) => key !== 'source_id'))];
      setDraft(JSON.stringify({...parsedDraft, versions}, null, 2)); setPriceDraft(null); setPriceInputs({});
      say(`已加入 ${versions.length} 个价格版本，填写原因后发布`, 'info');
    } catch (error) {say(error instanceof Error ? error.message : String(error));}
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
  } catch (error) {draftError = error instanceof SyntaxError ? '配置 JSON 格式无效，请检查逗号、引号和括号。' : String(error);}
  const rows = Array.isArray(parsedDraft[kind]) ? parsedDraft[kind].filter(row => row && typeof row === 'object' && !Array.isArray(row)) : [];
  const selectedRow = selected === null ? rows[0] : rows.find(row => row.id === selected);
  const editorRef = useRef<HTMLElement>(null);
  const originalRows = config?.[kind] ?? [];
  const isEdited = (row: Record<string, unknown>) => JSON.stringify(row) !== JSON.stringify(originalRows.find(original => original.id === row.id));
  const focusEditor = () => {editorRef.current?.scrollIntoView({block: 'start'}); editorRef.current?.focus({preventScroll: true});};
  const previewFields = ['fixed_input_credit_per_m', 'fixed_output_credit_per_m', 'fixed_cache_creation_credit_per_m', 'fixed_cache_read_credit_per_m'];
  const previewGroup = config?.groups.find(group => group.id === selectedRow?.group_id);
  let preview = '', previewError = '';
  if (priceDraft) {
    try {
      if (!selectedRow || !previewGroup) throw new Error('请选择模型并确认其所属分组');
      if (previewGroup.rate_card_id !== priceDraft.rate_card_id) throw new Error('当前模型的分组价格表与此草稿不匹配，请选择对应模型');
      if (![selectedRow.exposed_model_id, selectedRow.target_model, '*'].includes(priceDraft.model)) throw new Error('此价格草稿不匹配当前模型，暂不预览');
      if (String(priceDraft.margin_multiplier ?? '').trim() === '') throw new Error('请填写版本倍率');
      const charge = previewFixedCharge(previewFields.map(field => priceToMicroPerMillion(priceInputs[field] ?? '', priceUnit)), tokenInputs,
        [Number(priceDraft.margin_multiplier), Number(previewGroup.margin_multiplier), Number(selectedRow.credit_multiplier)]);
      preview = formatMicroPrice(charge) + ' 积分（' + charge + ' 微积分）';
    } catch (error) {previewError = error instanceof Error ? error.message : String(error);}
  }
  const multiplierChain = priceDraft ? `用量费用 × 版本 ${String(priceDraft.margin_multiplier ?? '—')} × 分组 ${String(previewGroup?.margin_multiplier ?? '—')} × 模型 ${String(selectedRow?.credit_multiplier ?? '—')}，最后向上取整到 1 微积分` : '';
  const historyCredits = (value: unknown) => {
    try {
      if (typeof value !== 'number') throw new Error('missing price');
      return formatMicroPrice(value);
    } catch {return '无法安全显示，请核对原始配置';}
  };
  const historyTime = (value: unknown) => {
    if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0) return '未提供有效时间';
    const date = new Date(value * 1000);
    return Number.isFinite(date.getTime()) ? formatFullDateTime(value) : '未提供有效时间';
  };
  const priceTime = Number(priceDraft?.effective_from_secs);
  const priceDate = new Date(priceTime * 1000);
  const localPriceTime = priceTime > 0 && Number.isFinite(priceDate.getTime()) ? new Date(priceDate.getTime() - priceDate.getTimezoneOffset() * 60000).toISOString().slice(0, 16) : '';
  const changePriceUnit = (unit: PriceUnit) => {
    try {
      const next = Object.fromEntries(Object.entries(priceInputs).map(([field, value]) => [field, formatMicroPrice(priceToMicroPerMillion(value, priceUnit), unit)]));
      setPriceInputs(next); setPriceUnit(unit);
    } catch (error) {say('切换单位前请修正售价：' + String(error));}
  };
  const updateField = (field: string, value: unknown) => {
    if (!selectedRow || rows.filter(row => row.id === selectedRow.id).length !== 1) {say('当前条目 ID 重复或不存在，请先修正高级配置；未修改任何条目。'); return;}
    const next = rows.map(row => row.id === selectedRow?.id ? {...row, [field]: value} : row);
    setDraft(JSON.stringify({...parsedDraft, [kind]: next}, null, 2));
  };
  const numericFields = ['virtual_usage_limit', 'margin_multiplier', 'context_window', 'max_output', 'credit_multiplier', 'rate_multiplier'];
  // Shown in the customer's model list; left empty, the server derives them.
  const optionalFields = ['display_name', 'description', 'rate_multiplier'];
  const tokenFields = ['context_window', 'max_output'];
  const checkFields = ['issuance_enabled', 'visible', 'supports_tools', 'supports_vision', 'supports_reasoning'];
  const fields = kind === 'groups' ? ['name', 'issuance_enabled', 'virtual_plan_name', 'virtual_usage_limit', 'rate_card_id', 'margin_multiplier'] : ['exposed_model_id', 'display_name', 'description', 'rate_multiplier', 'target_provider_id', 'target_model', 'group_id', 'context_window', 'max_output', 'credit_multiplier', 'visible', 'supports_tools', 'supports_vision', 'supports_reasoning'];
  const labels: Record<string, string> = {name: '名称', issuance_enabled: '可发新卡', virtual_plan_name: '对外套餐名', virtual_usage_limit: '显示用量上限', rate_card_id: '价格表', margin_multiplier: '扣费倍率', exposed_model_id: '模型 ID', target_provider_id: '供应商 ID', target_model: '上游模型', group_id: '分组', context_window: '上下文', max_output: '最大输出', credit_multiplier: '扣费倍率', display_name: '显示名称', description: '说明', rate_multiplier: '显示倍率', visible: '客户可见', supports_tools: '工具', supports_vision: '视觉', supports_reasoning: '推理'};
  // Accessible names the tests and screen readers already know; each contains its visible label.
  const ariaLabels: Record<string, string> = {context_window: '上下文长度', max_output: '最大输出', margin_multiplier: kind === 'groups' ? '分组扣费倍率' : '扣费倍率', credit_multiplier: '模型扣费倍率'};
  const tips: Record<string, string> = {issuance_enabled: '关闭后不能再发新卡，已发的卡不受影响', virtual_plan_name: '客户端显示的套餐名，与发卡套餐无关', virtual_usage_limit: '只在客户端显示，不是卡内积分', rate_multiplier: '只影响客户端显示，不影响扣费'};
  const placeholders: Record<string, string> = {display_name: '留空用模型 ID', description: '留空自动生成', rate_multiplier: '如 1.3，留空自动换算', context_window: '如 200K', max_output: '如 8K'};
  // An unpublished draft survives a session end, restored only onto the configuration it
  // was made from; the draft holds no secret.
  const draftKey = `admin-commercial-draft:v1:${kind}`;
  // `keepSaved` is false for a reload the operator asked for after discarding their edits:
  // the kept draft is dropped then, never brought back.
  const apply = (next: CommercialConfig, keepSaved: boolean): 'restored' | 'stale' | 'none' => {
    const value = JSON.stringify(kind === 'groups' ? {groups: next.groups} : {models: next.models, rate_cards: next.rate_cards, versions: []}, null, 2);
    let saved: {base?: unknown; draft?: unknown; reason?: unknown} = {};
    if (keepSaved) {try {saved = JSON.parse(sessionStorage.getItem(draftKey) || '{}') ?? {};} catch {/* nothing to restore */}}
    else {try {sessionStorage.removeItem(draftKey);} catch {/* nothing kept */}}
    const restore = saved.base === value && typeof saved.draft === 'string' && typeof saved.reason === 'string';
    setConfig(next); setServerChanged(false);
    setSelected(current => next[kind].some(row => String(row.id) === current) ? current : String(next[kind][0]?.id ?? '')); setPriceDraft(null); setPriceInputs({}); setReason(restore ? String(saved.reason) : ''); setLoadedDraft(value); setDraft(restore ? String(saved.draft) : value); setNeedsReview(false);
    return restore ? 'restored' : typeof saved.draft === 'string' ? 'stale' : 'none';
  };
  useEffect(() => {
    if (!loadedDraft) return;
    try {
      if (draft !== loadedDraft || reason.trim()) sessionStorage.setItem(draftKey, JSON.stringify({base: loadedDraft, draft, reason}));
      else sessionStorage.removeItem(draftKey);
    } catch {/* A draft that cannot be kept is only lost at a session end. */}
  }, [draft, loadedDraft, reason, draftKey]);
  const load = async (keepSaved = true, quiet = false) => {
    if (pending.current) return;
    pending.current = true; setBusy(true); setMessage('');
    try {
      const result = await adminApi.getCommercialConfig();
      if (result.success !== true || !result.config?.revision) throw new Error('服务器未确认配置读取成功');
      if (alive.current) {
        const draftState = apply(result.config, keepSaved);
        if (draftState === 'restored') say('已恢复未发布的修改，请核对后发布', 'info');
        else if (draftState === 'stale') say('配置已被更新，之前未发布的修改已作废', 'warning');
        else if (!keepSaved && !quiet) toast.success('已重新加载配置');
      }
    } catch (error) {
      if (alive.current) {setNeedsReview(true); say(`加载失败（${error instanceof Error ? error.message : String(error)}），修改已保留；重新加载成功前不能发布`);}
    } finally {pending.current = false; if (alive.current) setBusy(false);}
  };
  useEffect(() => {alive.current = true; void load(); return () => {alive.current = false;};}, [kind]);
  // A console refresh reloads this page too, but never over unpublished edits.
  const seenEpoch = useRef(refreshEpoch);
  useEffect(() => {
    if (refreshEpoch === seenEpoch.current) return;
    seenEpoch.current = refreshEpoch;
    if (pending.current) return;
    if (dirty) setServerChanged(true);
    else void load(false, true);
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refreshEpoch]);
  const editedRows = rows.filter(isEdited);
  const newVersions = Array.isArray(parsedDraft.versions) ? parsedDraft.versions.length : 0;
  const publish = async () => {
    if (!config || pending.current || needsReview) return;
    let submitted = false;
    try {
      if (priceDraft) throw new Error('价格编辑尚未加入发布草稿，请先点击“加入价格草稿”，或取消价格编辑后再发布。');
      if (draftError) throw new Error(draftError);
      if (!validText(reason.trim(), 500)) throw new Error('请填写变更原因（最多约 160 字，不含控制字符）');
      for (const [section, entries] of Object.entries(parsedDraft)) {
        if (section === 'settings') continue;
        const ids = new Set();
        for (const row of entries) {
          if (!validText(row.id, 128) || ids.has(row.id)) throw new Error(`${section} 中的 ID 不能为空、重复或超过 128 字节`);
          ids.add(row.id);
          if (section === 'groups') {
            if (!validText(row.name, 256) || ('virtual_plan_name' in row && !validText(row.virtual_plan_name, 256))) throw new Error('名称和对外套餐名不能为空，且不得超过 256 字节');
            if ('margin_multiplier' in row && !positive(row.margin_multiplier)) throw new Error('分组扣费倍率需大于 0、不超过 1000，空白不能作为 0');
            if ('virtual_usage_limit' in row && (typeof row.virtual_usage_limit !== 'number' || !Number.isFinite(row.virtual_usage_limit) || row.virtual_usage_limit < 0)) throw new Error('显示用量上限需为非负数，不能留空');
          }
          if (section === 'models') {
            if (!validText(row.exposed_model_id, 128) || !validText(row.target_model, 256) || !validText(row.target_provider_id, 256) || !validText(row.group_id, 128)) throw new Error('请填写有效的模型 ID、上游模型、供应商和分组');
            if (!positive(row.credit_multiplier)) throw new Error('模型扣费倍率需大于 0、不超过 1000，空白不能作为 0');
            if (!Number.isSafeInteger(row.context_window) || Number(row.context_window) < 1 || Number(row.context_window) > 10_000_000 || !Number.isSafeInteger(row.max_output) || Number(row.max_output) < 1 || Number(row.max_output) > Number(row.context_window)) throw new Error('上下文长度须为 1 至 10,000,000 的整数；最大输出须为正整数且不能超过上下文长度');
            const old = config.models.find(model => model.id === row.id);
            if (old && old.group_id !== row.group_id) throw new Error('已有模型不能改分组；请在 JSON 中用新的映射 ID 新增条目');
          }
          if (section === 'versions') {
            if (config.versions.some(version => version.id === row.id)) throw new Error('版本 ID 已存在，不能覆盖历史价格');
            if (!Number.isSafeInteger(row.effective_from_secs) || Number(row.effective_from_secs) <= Date.now() / 1000) throw new Error('价格生效时间需晚于现在，请在 JSON 中修正草稿时间');
            if (!positive(row.margin_multiplier)) throw new Error('版本倍率需大于 0、不超过 1000');
          }
        }
      }
      const names = editedRows.map(row => String(row.name ?? row.exposed_model_id ?? row.id));
      const changes = names.length + newVersions;
      const confirmed = await confirmAction({
        title: changes ? `发布 ${changes} 项修改？` : `发布${kind === 'groups' ? '分组' : '模型与价格'}配置？`,
        facts: [
          ...(names.length ? [`修改：${names.slice(0, 5).join('、')}${names.length > 5 ? ` 等 ${names.length} 项` : ''}`] : []),
          ...(newVersions ? [`${newVersions} 个新价格版本按指定时间生效`] : []),
          `原因：${reason.trim()}`,
          `基于版本 ${shortHash(config.revision)}`,
        ],
        consequence: '发布后新请求立即生效。',
        confirmLabel: '发布',
      });
      if (!confirmed || !alive.current || pending.current) return;
      pending.current = true; submitted = true; setBusy(true); setPublishing(true); setMessage('');
      const result = await adminApi.publishCommercialConfig({...parsedDraft, expected_revision: config.revision, reason: reason.trim()});
      if (result.success !== true) throw new AdminApiError('服务器未确认发布成功', 400);
      if (!result.config?.revision) throw new Error('服务器未返回可核对的配置版本');
      try {sessionStorage.removeItem(draftKey);} catch {/* published; nothing left to keep */}
      if (alive.current) {apply(result.config, false); toast.success('已发布'); onPublished?.();}
    } catch (error) {
      if (alive.current) {
        const mustReview = submitted && !(error instanceof AdminApiError && [400, 401, 403, 413, 422].includes(error.status));
        if (mustReview) setNeedsReview(true);
        const text = error instanceof Error ? error.message : String(error);
        say(mustReview ? `没收到发布结果（${text}）。修改已保留，请重新加载确认后再发布，不要重复提交。` : text);
      }
    } finally {if (submitted) {pending.current = false; if (alive.current) {setBusy(false); setPublishing(false);}}}
  };
  const selectTemplate = async (id: string) => {
    if (priceDraft && !(await confirmAction({title: '放弃当前调价？', consequence: '还没加入草稿的价格修改会丢失。', confirmLabel: '放弃'}))) return;
    const version = config?.versions.find(value => value.id === id);
    if (!version) {setPriceDraft(null); setPriceInputs({}); return;}
    try {
      const inputs = Object.fromEntries(Object.keys(priceFields).map(field => [field, formatMicroPrice(Number(version[field]), priceUnit)]));
      setPriceDraft({...version, id: '', effective_from_secs: 0, source_id: version.id}); setPriceInputs(inputs);
    } catch (error) {say('模板价格无法安全读取，原编辑已保留：' + String(error));}
  };
  const cancelPrice = async () => {
    if (await confirmAction({title: '放弃当前调价？', consequence: '还没加入草稿的价格修改会丢失。', confirmLabel: '放弃'})) {setPriceDraft(null); setPriceInputs({});}
  };
  const reload = async () => {
    if (dirty && !(await confirmAction({title: '放弃未发布的修改？', consequence: '会重新加载服务器上的配置。', confirmLabel: '放弃修改'}))) return;
    void load(false);
  };
  const rateCards = config?.rate_cards ?? [], configGroups = config?.groups ?? [], configModels = config?.models ?? [], configVersions = config?.versions ?? [];
  const rateCardName = (id: unknown) => String(rateCards.find(card => card.id === id)?.name ?? id ?? '—');
  const groupName = (id: unknown) => String(configGroups.find(group => group.id === id)?.name ?? id ?? '—');
  const cardCount = (groupId: unknown) => cards ? cards.filter(card => card.groupId === groupId && card.status !== 'voided' && card.archivedAt == null).length : null;
  const existingModel = kind === 'models' && !!selectedRow && configModels.some(model => model.id === selectedRow.id);
  const nowSecs = Date.now() / 1000;
  const publishBlocked = needsReview ? '请重新加载确认后再发布' : !config ? '配置没有加载' : !reason.trim() ? '填写变更原因后可发布' : undefined;
  const editorFields = selectedRow ? fields.filter(field => checkFields.includes(field) ? field === 'issuance_enabled' || field in selectedRow : field in selectedRow) : [];

  const renderInput = (field: string) => {
    if (!selectedRow) return null;
    const label = labels[field];
    const aria = ariaLabels[field] ?? label;
    if (field === 'rate_card_id' && rateCards.length) {
      const value = String(selectedRow[field] ?? '');
      return <select aria-label={aria} value={value} onChange={event => updateField(field, event.target.value)}>
        {!rateCards.some(card => card.id === value) && <option value={value}>{value || '请选择'}</option>}
        {rateCards.map(card => <option key={String(card.id)} value={String(card.id)}>{String(card.name ?? card.id)}</option>)}
      </select>;
    }
    if (field === 'group_id' && configGroups.length) {
      const value = String(selectedRow[field] ?? '');
      return <select aria-label={aria} value={value} disabled={existingModel} title={existingModel ? '已有模型不能改分组' : undefined} onChange={event => updateField(field, event.target.value)}>
        {!configGroups.some(group => group.id === value) && <option value={value}>{value || '请选择'}</option>}
        {configGroups.map(group => <option key={String(group.id)} value={String(group.id)}>{String(group.name ?? group.id)}</option>)}
      </select>;
    }
    const token = tokenFields.includes(field);
    const multiplier = ['margin_multiplier', 'credit_multiplier'].includes(field);
    const input = <input aria-label={aria} placeholder={placeholders[field]} type={numericFields.includes(field) && !token ? 'number' : 'text'} step={token ? '1' : 'any'}
      value={String(selectedRow[field] ?? '')}
      onChange={event => updateField(field, optionalFields.includes(field) && !event.target.value.trim() ? null : token ? parseTokenInput(event.target.value) : numericFields.includes(field) && event.target.value.trim() ? Number(event.target.value) : event.target.value)}/>;
    return multiplier ? <span className="input-suffix">{input}<span>×</span></span> : input;
  };

  return <div className="page-stack commercial-editor">
    <section className="panel">
      <div className="table-scroll"><table className="table">
        <thead><tr>{(kind === 'groups' ? ['名称', '可发卡', '对外套餐名', '用量上限', '价格表', '倍率', '卡密数', ''] : ['模型', '显示名', '上游', '分组', '上下文 / 输出', '客户可见', '']).map((label, index) =>
          <th key={index} className={['用量上限', '倍率', '卡密数', '上下文 / 输出'].includes(label) ? 'num' : label ? undefined : 'col-actions'}>{label || <span className="sr-only">操作</span>}</th>)}</tr></thead>
        <tbody>
          {rows.map((row, index) => {
            const active = selectedRow?.id === row.id;
            const edited = isEdited(row);
            const cells = kind === 'groups'
              ? [<td key="n" className="cell-strong">{String(row.name ?? row.id)}{edited && <span className="edited-dot">已修改</span>}</td>,
                <td key="i">{row.issuance_enabled === false ? <span className="muted">—</span> : '✓'}</td>,
                <td key="p">{String(row.virtual_plan_name ?? '—')}</td>,
                <td key="u" className="num">{typeof row.virtual_usage_limit === 'number' ? formatCount(row.virtual_usage_limit) : '—'}</td>,
                <td key="r" title={String(row.rate_card_id ?? '')}>{rateCardName(row.rate_card_id)}</td>,
                <td key="m" className="num">{String(row.margin_multiplier ?? '—')}</td>,
                <td key="c" className="num">{cardCount(row.id) ?? '—'}</td>]
              : [<td key="n" className="cell-strong mono">{String(row.exposed_model_id ?? row.id)}{edited && <span className="edited-dot">已修改</span>}</td>,
                <td key="d">{String(row.display_name ?? '') || <span className="muted">—</span>}</td>,
                <td key="t" className="mono">{String(row.target_provider_id ?? '—')} / {String(row.target_model ?? '—')}</td>,
                <td key="g" title={String(row.group_id ?? '')}>{groupName(row.group_id)}</td>,
                <td key="w" className="num">{formatTokenShort(row.context_window)} / {formatTokenShort(row.max_output)}</td>,
                <td key="v">{row.visible === false ? <span className="muted">隐藏</span> : '✓'}</td>];
            return <tr key={String(row.id ?? index)} className={active ? 'is-selected' : undefined}>
              {cells}
              <td className="col-actions"><button type="button" className="btn-text" disabled={busy} onClick={() => {setSelected(String(row.id)); focusEditor();}}>编辑</button></td>
            </tr>;
          })}
          {!rows.length && <tr className="state-row"><td colSpan={8}>{busy ? <div className="skeleton" role="status" aria-label="正在加载"><span className="skeleton-bar"/><span className="skeleton-bar"/></div> : <div className="list-state"><p>暂无数据</p></div>}</td></tr>}
        </tbody>
      </table></div>
    </section>

    <section ref={editorRef} tabIndex={-1} className="panel mapping-editor">
      <h3>编辑：{String(selectedRow?.name ?? selectedRow?.exposed_model_id ?? '请选择条目')}</h3>
      <fieldset disabled={busy || !selectedRow} className="form-grid form-grid-3">
        {editorFields.filter(field => !checkFields.includes(field)).map(field => <label key={field} className="field">
          <span className="field-label">{labels[field]}{tips[field] && <InfoTip text={tips[field]}/>}</span>
          {renderInput(field)}
          {tokenFields.includes(field) && selectedRow && <span className="field-hint" title={formatTokens(selectedRow[field])}>{typeof selectedRow[field] === 'number' && Number(selectedRow[field]) > 0 ? `${formatTokenCount(Number(selectedRow[field]))} Tokens` : '请输入正整数 Tokens'}</span>}
        </label>)}
        {editorFields.some(field => checkFields.includes(field)) && <div className="field field-span check-row">
          {editorFields.filter(field => checkFields.includes(field)).map(field => <span key={field} className="check-field">
            <label><input type="checkbox" checked={field === 'issuance_enabled' ? selectedRow?.[field] !== false : Boolean(selectedRow?.[field])} onChange={event => updateField(field, event.target.checked)}/>{labels[field]}</label>
            {tips[field] && <InfoTip text={tips[field]}/>}
          </span>)}
        </div>}
      </fieldset>
    </section>

    {kind === 'models' && <section className="panel pricing-editor">
      <div className="pricing-heading"><h3>积分价格</h3></div>
      <div className="pricing-template"><label>选择价格版本模板<select disabled={busy} aria-label="选择价格版本模板" value={String(priceDraft?.source_id ?? '')} onChange={e => void selectTemplate(e.target.value)}><option value="">从现有版本创建新价格</option>{configVersions.filter(v => v.pricing_mode === 'fixed').map(v => <option key={String(v.id)} value={String(v.id)}>{String(v.model)} · {String(v.id)}</option>)}</select></label></div>
      {priceDraft && <fieldset disabled={busy} className="pricing-form">
        <div className="pricing-grid pricing-meta">
          <label>新版本 ID<input value={String(priceDraft.id)} onChange={e => setPriceDraft({...priceDraft, id: e.target.value})}/></label>
          <label>生效时间（本地时区）<input type="datetime-local" value={localPriceTime} onChange={e => setPriceDraft({...priceDraft, effective_from_secs: Math.floor(new Date(e.target.value).getTime() / 1000)})}/></label>
          <label>客户售价单位<select value={priceUnit} onChange={e => changePriceUnit(e.target.value as PriceUnit)}><option value="million">积分 / 百万 Tokens</option><option value="thousand">积分 / 千 Tokens</option></select></label>
          <label>版本倍率<input aria-label="版本倍率" inputMode="decimal" value={String(priceDraft.margin_multiplier ?? '')} onChange={e => setPriceDraft({...priceDraft, margin_multiplier: e.target.value})}/></label>
        </div>
        <div className="pricing-grid pricing-rates">{Object.entries(priceFields).map(([field, label]) => <label key={field}>{label}售价（积分 / {priceUnit === 'million' ? '百万' : '千'} Tokens）<input inputMode="decimal" value={priceInputs[field] ?? ''} onChange={e => setPriceInputs({...priceInputs, [field]: e.target.value})}/></label>)}</div>
        <section className="pricing-preview" aria-label="客户扣费预览">
          <div className="pricing-heading"><h4>扣费示例</h4><span className="muted">模型 {String(selectedRow?.exposed_model_id ?? '未选择')} · 分组 {String(previewGroup?.name ?? previewGroup?.id ?? '未找到')}</span></div>
          <div className="pricing-grid pricing-tokens">{['未缓存输入', '输出', '缓存写入', '缓存读取'].map((label, index) => <label key={label}>{label} Tokens<input inputMode="numeric" value={tokenInputs[index]} onChange={e => setTokenInputs(values => values.map((value, i) => i === index ? e.target.value : value))}/></label>)}</div>
          <p role="status" title={previewError ? undefined : multiplierChain} className={'pricing-result' + (previewError ? ' pricing-result-error' : '')}>{previewError || <>预计扣费：<strong>{preview}</strong></>}</p>
        </section>
        <section className="pricing-procurement" aria-label="采购参考价格">
          <div className="pricing-heading"><h4>采购价（用于成本估算）</h4><label>采购计价币种<select value={String(priceDraft.currency ?? '')} onChange={e => setPriceDraft({...priceDraft, currency: e.target.value})}><option value="">请选择</option><option value="USD">USD</option><option value="CNY">CNY</option></select></label></div>
          <div className="pricing-grid">{Object.entries(costFields).map(([field, label]) => <label key={field}>{label}（计价货币 / 百万 Tokens）<input type="number" min="0" step="any" value={String(priceDraft[field] ?? '')} onChange={e => setPriceDraft({...priceDraft, [field]: e.target.value})}/></label>)}</div>
        </section>
        <div className="pricing-actions"><button type="button" className="btn btn-primary" onClick={stagePrice}>加入价格草稿</button><button type="button" className="btn" onClick={() => void cancelPrice()}>取消价格编辑</button></div>
      </fieldset>}
    </section>}

    {kind === 'models' && <section className="panel" aria-label="历史价格版本">
      <h3>价格版本</h3>
      <div className="table-scroll"><table className="table price-history"><thead><tr><th>版本 / 模型 / 价格表</th><th>客户基准售价（倍率前）</th><th className="num">版本倍率</th><th>采购参考价</th><th>生效时间</th><th className="col-status">状态</th></tr></thead><tbody>
        {configVersions.map(version => <tr key={String(version.id)}>
          <td><span className="mono">{String(version.id)}</span><br/><span className="muted">{String(version.model)} · {String(version.rate_card_id)}</span></td>
          <td>{version.pricing_mode === 'fixed' ? <><span className="muted">固定积分 · 积分 / 百万 Tokens</span>{Object.entries(priceFields).map(([field, label]) => <div key={field}>{label}：{historyCredits(version[field])}</div>)}</>
            : version.pricing_mode === 'per_call' ? <>按次计费：{historyCredits(version.per_call_credit)} 积分 / 次</>
            : version.pricing_mode === 'cost_plus' ? <>成本加成（非固定积分售价）：按采购价、汇率、积分面值和倍率计费</>
            : <>未知计费模式：{String(version.pricing_mode ?? '未提供')}</>}</td>
          <td className="num">{typeof version.margin_multiplier === 'number' && Number.isFinite(version.margin_multiplier) && version.margin_multiplier >= 0 ? String(version.margin_multiplier) : '—'}</td>
          <td>{['USD', 'CNY'].includes(String(version.currency)) ? <><span className="muted">{String(version.currency)} / 百万 Tokens</span>{Object.entries(costFields).map(([field, label]) => <div key={field}>{label}：{typeof version[field] === 'number' && Number.isFinite(version[field]) && Number(version[field]) >= 0 ? String(version[field]) : '—'}</div>)}</> : '—'}</td>
          <td>{historyTime(version.effective_from_secs)}</td>
          <td className="col-status"><StatusBadge view={priceVersionView(version, configVersions, nowSecs)}/></td>
        </tr>)}
        {!configVersions.length && <tr><td colSpan={6}>暂无价格版本</td></tr>}
      </tbody></table></div>
    </section>}

    <details className="panel json-details">
      <summary>编辑 JSON（高级）</summary>
      <p className="muted">按 ID 新增或更新；删掉一行不会删除服务器上的条目。</p>
      <textarea aria-label="配置 JSON" value={draft} disabled={busy} onChange={e => setDraft(e.target.value)} spellCheck={false} className="code-input"/>
      {config && draftError && <p role="alert" className="form-error">{draftError} 修正后才能加入价格或发布。</p>}
    </details>
    <details className="panel json-details">
      <summary>查看原始配置（只读）</summary>
      <pre className="code-block">{JSON.stringify(config, null, 2)}</pre>
    </details>

    <div className={`action-bar${dirty ? ' is-dirty' : ''}`} role="region" aria-label="发布">
      <div className="action-bar-main">
        <span className="action-bar-status">
          {editedRows.length > 0 && <span className="dirty-dot">{editedRows.length} 项修改未发布</span>}
          {newVersions > 0 && <span className="dirty-dot">{newVersions} 个新价格版本</span>}
          {needsReview && <span className="is-warning">请重新加载确认后再发布</span>}
          {serverChanged && !needsReview && <span className="is-warning">服务器上的配置可能已更新</span>}
        </span>
        <input className="action-bar-reason" aria-label="变更原因" placeholder="变更原因（必填）" value={reason} maxLength={500} disabled={busy} onChange={e => setReason(e.target.value)}/>
        {/* Discarding is offered only when there is something to discard (or to review). */}
        {(dirty || needsReview || serverChanged || !config) && <button type="button" className="btn" disabled={busy} onClick={() => void reload()}>
          {dirty ? (serverChanged && !needsReview ? '放弃修改并加载' : '放弃修改') : '重新加载'}</button>}
        <button type="button" className="btn btn-primary" disabled={busy || !!publishBlocked} title={publishBlocked} onClick={() => void publish()}>{publishing ? '发布中…' : '发布'}</button>
      </div>
      {message && <p role="status" className={`message message-${messageTone}`}>{message}</p>}
    </div>
  </div>;
}

function formatTokenShort(value: unknown): string {
  if (typeof value !== 'number' || !Number.isFinite(value)) return '—';
  if (value >= 1_000_000) return `${+(value / 1_000_000).toFixed(1)}M`;
  if (value >= 1_000) return `${+(value / 1_000).toFixed(1)}K`;
  return String(value);
}
