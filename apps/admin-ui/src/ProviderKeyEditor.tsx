import {useEffect, useRef, useState} from 'react';
import {adminApi, AdminApiError} from './api';
import {ask, confirmAction} from './components/confirm';
import {IconClose} from './components/icons';
import {toast} from './components/toast';
import {InfoTip} from './components/ui';
import Probe from './Probe';
import {explainRefusal, isRefusal, type PublishOutcome} from './refusal';
import {lossFacts, modelName, nameList, routeLosses, type RouteLosses} from './routes';

type Row = Record<string, unknown>;
type Message = {tone: 'error' | 'warning' | 'info'; text: string} | null;
const NO_LOSSES: RouteLosses = {down: [], takeover: [], backup: []};

export default function ProviderKeyEditor({selectedKey, preset, knownModels = [], modelsKnown = knownModels.length > 0, knownProviders = [], routes, onHideModels, onSaved, onDeleted, onDirtyChange, onBusyChange, onClose, onListModel}: {
  selectedKey?: Record<string, unknown>;
  preset?: {providerId?: string; keyId?: string; suggestedKeyId?: string};
  /** This provider's upstream models already listed in 模型与定价, to point out the ones that are not. */
  knownModels?: string[];
  /** Whether 模型与定价 loaded (otherwise nothing is marked 未上架). */
  modelsKnown?: boolean;
  /** 上架 for a model this Key is saved as authorised for. */
  onListModel?: (providerId: string, model: string) => void;
  /** IDs of the providers that already exist. */
  knownProviders?: string[];
  /** Every model, group, provider and Key: to say which shown models a change leaves without a route. */
  routes?: {models: Row[]; groups: Row[]; providers: Row[]; keys: Row[]};
  /** 同时隐藏这些模型: publishes these models hidden, before the change that leaves them without a route. */
  onHideModels?: (models: Row[], reason: string) => Promise<PublishOutcome>;
  onSaved?: (savedKey?: Record<string, unknown>) => void;
  onDeleted?: () => void;
  onDirtyChange: (dirty: boolean) => void;
  onBusyChange: (busy: boolean) => void;
  onClose?: () => void;
}) {
  const [provider, setProvider] = useState(preset?.providerId ?? '');
  const [baseUrl, setBaseUrl] = useState('');
  const [format, setFormat] = useState('anthropic');
  const [keyId, setKeyId] = useState(preset?.keyId ?? preset?.suggestedKeyId ?? '');
  const [secret, setSecret] = useState('');
  // Uncontrolled so the key never lands in the input's DOM attribute.
  const secretInput = useRef<HTMLInputElement>(null);
  useEffect(() => {if (!secret && secretInput.current) secretInput.current.value = '';}, [secret]);
  const [models, setModels] = useState('');
  // The list is ticked, not typed; typing stays available behind 手动输入.
  const [manual, setManual] = useState(false);
  const [modelQuery, setModelQuery] = useState('');
  const [seen, setSeen] = useState<string[]>([]);
  const [weight, setWeight] = useState('1');
  const [enabled, setEnabled] = useState(true);
  const [busy, setBusy] = useState(false);
  const [dirty, setDirty] = useState(false);
  const pending = useRef(false);
  const [review, setReview] = useState<{provider: string; key: string} | null>(null);
  const [message, setMessage] = useState<Message>(null);
  const mode: 'provider' | 'add-key' | 'edit' = selectedKey ? 'edit' : preset?.providerId ? 'add-key' : 'provider';
  const providerExists = knownProviders.includes(provider.trim());
  // A new provider gets <provider>-key-1 (the Key the import creates); discovery uses that Key.
  const workingKeyId = mode === 'provider' ? (provider.trim() ? `${provider.trim()}-key-1` : '') : keyId.trim();
  const modelIds = [...new Set(models.split(/\r?\n/).map(model => model.trim()).filter(Boolean))];
  const validText = (text: string, max: number) => !!text.trim() && new TextEncoder().encode(text).length <= max && !/[\x00-\x1f\x7f-\x9f]/.test(text);
  const validate = (importing = false) => {
    if (!validText(provider.trim(), 256)) throw new Error('请填写供应商 ID（不超过 256 字节，不含控制字符）');
    if (!importing && !validText(workingKeyId, 256)) throw new Error('请填写 Key ID（不超过 256 字节，不含控制字符）');
    if ((secret || importing) && (!validText(secret, 4096) || secret !== secret.trim())) throw new Error('请填写 API Key（首尾不能有空格，不超过 4096 字节）');
  };
  const validateModels = () => {
    if (modelIds.length > 1000 || modelIds.some(model => !validText(model, 256))) throw new Error('最多 1000 个模型，每个 ID 不超过 256 字节');
  };
  const writeFailure = (error: unknown, target: {provider: string; key: string}, action = '保存') => {
    const rejected = isRefusal(error);
    if (!rejected) setReview(target);
    const text = error instanceof Error ? error.message : String(error);
    setMessage({tone: 'error', text: rejected ? `${action}失败：${explainRefusal(text, id => nameOfId(id))}。修改已保留，请修正后再${action}。`
      : `没收到${action}结果（${text}），已暂停保存、删除和导入。请重新读取确认后再操作，不要重复提交。`});
  };
  useEffect(() => {onDirtyChange(dirty);}, [dirty, onDirtyChange]);
  useEffect(() => {onBusyChange(busy); return () => onBusyChange(false);}, [busy, onBusyChange]);
  useEffect(() => () => onDirtyChange(false), [onDirtyChange]);
  const [discovery, setDiscovery] = useState<{key: string; models: string[]; incomplete: boolean} | null>(null);
  useEffect(() => {
    if (!selectedKey) return;
    setProvider(String(selectedKey.provider_id ?? '')); setKeyId(String(selectedKey.id ?? ''));
    setModels(Array.isArray(selectedKey.allowed_models) ? selectedKey.allowed_models.join('\n') : '');
    setWeight(String(selectedKey.weight ?? 1)); setEnabled(selectedKey.enabled !== false); setSecret(''); setDiscovery(null);
  // Refreshes must not replace an in-progress draft; remounting reads the latest saved row.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedKey?.id, selectedKey?.provider_id]);
  useEffect(() => {setDiscovery(null);}, [provider, keyId, secret]);

  const savedModels = Array.isArray(selectedKey?.allowed_models) ? (selectedKey!.allowed_models as unknown[]).map(String) : null;
  const nameOf = (model: Row) => routes ? modelName(model, routes.models, routes.groups) : String(model.exposed_model_id ?? model.id);
  const nameOfId = (id: string) => {const model = routes?.models.find(row => row.id === id || row.exposed_model_id === id); return model ? nameOf(model) : id;};
  /** What saving this Key as `next` (or, with null, deleting it) would take from the models customers see. */
  const lossesIf = (next: Row | null): RouteLosses => {
    if (!routes || mode !== 'edit') return NO_LOSSES;
    const same = (key: Row) => key.id === keyId.trim() && key.provider_id === provider.trim();
    const keys = next ? routes.keys.map(key => same(key) ? {...key, ...next} : key) : routes.keys.filter(key => !same(key));
    return routeLosses(routes.models, {providers: routes.providers, keys: routes.keys}, {providers: routes.providers, keys});
  };
  /** Hides the models first (同时隐藏这些模型); when that fails, says so and that nothing else was done. */
  const hideFirst = async (models: Row[], action: string): Promise<string | null> => {
    const names = nameList(models.map(nameOf));
    const outcome = await onHideModels!(models, `${action} Key ${keyId.trim()} 前隐藏将无可用线路的模型：${names}`);
    if (outcome.ok) return names;
    setMessage({tone: 'error', text: outcome.uncertain ? `没收到隐藏结果（${outcome.message}），Key 没有${action}。请点页面右上角的“刷新”核对这些模型是否已隐藏，再重新操作，不要重复提交。`
      : `没能隐藏这些模型：${outcome.message}。Key 没有${action}。`});
    return null;
  };
  const act = async (action: 'save' | 'discover') => {
    if (pending.current || (action === 'save' && review)) return;
    try {
      validate();
      if (action === 'save') {
        validateModels();
        if (!weight.trim() || !Number.isInteger(Number(weight)) || Number(weight) < 1 || Number(weight) > 1000) throw new Error('权重须为 1 至 1000 的整数');
      }
    } catch (error) {setMessage({tone: 'error', text: error instanceof Error ? error.message : String(error)}); return;}
    let hide: Row[] = [];
    if (action === 'save') {
      const added = savedModels ? modelIds.filter(model => !savedModels.includes(model)).length : modelIds.length;
      const losses = lossesIf({allowed_models: modelIds, enabled});
      const answer = await ask({
        title: `保存 Key ${keyId.trim()}？`,
        facts: [
          savedModels ? `可用模型 ${savedModels.length} → ${modelIds.length}${added ? `（新增 ${added}）` : ''}` : `可用模型 ${modelIds.length} 个`,
          `权重 ${weight} · ${enabled ? '启用' : '停用'}`,
          secret ? '将写入这次填写的 API Key' : selectedKey ? '密钥不变' : '新增 Key 必须填写 API Key',
          ...lossFacts(losses, nameOf),
        ],
        option: onHideModels && losses.down.length ? {label: `同时隐藏将无可用线路的 ${losses.down.length} 个模型（先隐藏，再保存）`} : undefined,
        consequence: [modelIds.length ? '' : '未选择模型：这个 Key 不会被使用。',
          losses.down.length ? `不隐藏的话，这 ${losses.down.length} 个模型仍在客户的模型列表里，但请求会失败。` : ''].filter(Boolean).join(' ') || undefined,
        confirmLabel: '保存',
        danger: losses.down.length > 0,
      });
      if (!answer.confirmed || pending.current) return;
      if (answer.option) hide = losses.down;
    }
    pending.current = true; setBusy(true);
    if (action === 'discover') setDiscovery(null);
    let hidden: string | null = null;
    try {
      // A provider that does not exist yet has no Key to save: 保存供应商 creates both.
      if (action === 'save' && mode === 'provider') throw new AdminApiError('请先保存供应商', 400);
      if (hide.length && !(hidden = await hideFirst(hide, '保存'))) return;
      const result = await adminApi.manageKey(action, {
        provider_id: provider.trim(), key_id: workingKeyId, ...(secret ? {api_key: secret} : {}),
        ...(action === 'save' ? {allowed_models: modelIds, weight: Number(weight), enabled} : {}),
      });
      if (result.success !== true) throw new AdminApiError('服务器未确认操作成功', 400);
      if (action === 'discover') {
        if (!Array.isArray(result.models) || result.models.some(model => typeof model !== 'string' || !validText(model, 256))) throw new Error('候选模型格式无效，可用模型没有改动');
        const candidates = [...new Set(result.models)];
        const fresh = candidates.filter(model => !modelIds.includes(model)).length;
        setDiscovery({key: workingKeyId, models: candidates, incomplete: Boolean(result.has_more)});
        setMessage(!candidates.length ? {tone: 'info', text: '上游没有返回模型，可以手动添加'}
          : result.has_more ? {tone: 'warning', text: `只取到部分模型（上游有分页，${candidates.length} 个），可手动补充`}
          : {tone: 'info', text: `获取到 ${candidates.length} 个模型（${fresh} 个新）`});
      } else {
        setDirty(false); setSecret(''); setMessage(null);
        const unlisted = modelIds.filter(model => !knownModels.includes(model)).length;
        toast.success(hidden ? `已隐藏 ${hidden}，并保存 Key ${keyId.trim()}` : unlisted && modelsKnown ? `已保存 Key ${keyId.trim()}，${unlisted} 个模型还没在“模型与定价”上架` : `已保存 Key ${keyId.trim()}`);
        onSaved?.({id: keyId.trim(), provider_id: provider.trim(), allowed_models: modelIds, weight: Number(weight), enabled});
      }
    } catch (error) {
      if (action === 'save') writeFailure(error, {provider: provider.trim(), key: keyId.trim()});
      else setMessage({tone: 'error', text: `获取失败（${error instanceof Error ? error.message : String(error)}），可用模型没有改动，可以重试`});
    } finally {pending.current = false; setBusy(false);}
  };

  const createProvider = async () => {
    if (pending.current || review) return;
    try {
      validate(true); validateModels();
      let url: URL;
      try {url = new URL(baseUrl.trim());} catch {throw new Error('请填写完整的上游地址，例如 https://api.example.com');}
      if (!(url.protocol === 'https:' || (url.protocol === 'http:' && ['localhost', '127.0.0.1'].includes(url.hostname)))) throw new Error('上游地址须使用 HTTPS（本机 localhost、127.0.0.1 可用 HTTP）');
    } catch (error) {setMessage({tone: 'error', text: error instanceof Error ? error.message : String(error)}); return;}
    const defaultKey = `${provider.trim()}-key-1`;
    const confirmed = await confirmAction({
      title: `添加供应商 ${provider.trim()}？`,
      facts: [`地址 ${baseUrl.trim()} · ${format === 'openai' ? 'OpenAI' : 'Anthropic'} 接口`, `将创建 Key：${defaultKey}（${modelIds.length} 个模型）`, ...(modelIds.length ? [] : ['未选择模型：这个 Key 不会被使用'])],
      consequence: providerExists ? `将覆盖 ${provider.trim()} 的地址和 ${defaultKey}，并重置为启用、权重 1。` : undefined,
      confirmLabel: '添加',
    });
    if (!confirmed || pending.current) return;
    pending.current = true; setBusy(true);
    try {
      const result = await adminApi.importProvider({providers: [{id: provider.trim(), name: provider.trim(), api_type: format, base_url: baseUrl.trim(), api_key: secret, models: modelIds}]});
      if (result.success !== true) throw new AdminApiError('服务器未确认供应商已保存', 400);
      setDirty(false); setKeyId(defaultKey); setWeight('1'); setEnabled(true); setSecret(''); setMessage(null);
      toast.success(`已添加供应商 ${provider.trim()}，Key ${defaultKey} 已启用`);
      onSaved?.();
    } catch (error) {writeFailure(error, {provider: provider.trim(), key: defaultKey});}
    finally {pending.current = false; setBusy(false);}
  };

  const remove = async () => {
    if (pending.current || review || mode !== 'edit') return;
    const target = {provider: provider.trim(), key: keyId.trim()};
    const losses = lossesIf(null);
    const answer = await ask({
      title: `删除 Key ${target.key}？`,
      facts: [`${target.provider} 的 Key · 可用模型 ${savedModels ? `${savedModels.length} 个` : '不限（旧版）'}`, ...lossFacts(losses, nameOf)],
      option: onHideModels && losses.down.length ? {label: `同时隐藏将无可用线路的 ${losses.down.length} 个模型（先隐藏，再删除）`, checked: true} : undefined,
      consequence: `删除后不能恢复：要再用，需重新添加 Key 并填写 API Key。${losses.down.length ? '在售模型只剩这条线路时，不隐藏它们服务器会拒绝删除。' : ''}`,
      confirmLabel: '删除', danger: true,
    });
    if (!answer.confirmed || pending.current) return;
    pending.current = true; setBusy(true);
    let hidden: string | null = null;
    try {
      if (answer.option && !(hidden = await hideFirst(losses.down, '删除'))) return;
      const result = await adminApi.deleteKey(target.provider, target.key);
      if (result.success !== true) throw new AdminApiError('服务器未确认删除', 400);
      setDirty(false); setMessage(null);
      toast.success(hidden ? `已隐藏 ${hidden}，并删除 Key ${target.key}` : `已删除 Key ${target.key}`);
      onDeleted?.();
    } catch (error) {writeFailure(error, target, '删除');}
    finally {pending.current = false; setBusy(false);}
  };

  const reviewKey = async () => {
    if (pending.current || !review) return;
    if (!(await confirmAction({title: '重新读取这个 Key？', consequence: '会覆盖未保存的修改并清空密钥输入。', confirmLabel: '重新读取'}))) return;
    if (pending.current) return;
    pending.current = true; setBusy(true);
    try {
      const result = await adminApi.getProviders();
      if (result.success !== true || !Array.isArray(result.keys)) throw new Error('Key 列表读取未确认');
      const key = result.keys.find(row => row.id === review.key && row.provider_id === review.provider);
      setProvider(review.provider); setKeyId(review.key); setSecret(''); setDiscovery(null);
      if (key) {setModels(Array.isArray(key.allowed_models) ? key.allowed_models.join('\n') : ''); setWeight(String(key.weight ?? 1)); setEnabled(key.enabled !== false);}
      setReview(null); setDirty(!key);
      if (key) onSaved?.(key);
      setMessage(key ? {tone: 'info', text: '已重新读取 Key 权限，请核对后再编辑'} : {tone: 'warning', text: '服务器上没有这个 Key。模型列表已保留，密钥输入已清空；请检查供应商和 Key ID'});
    } catch (error) {setMessage({tone: 'error', text: `${error instanceof Error ? error.message : String(error)}。核对失败，仍禁止重复保存或导入。`});}
    finally {pending.current = false; setBusy(false);}
  };

  const title = mode === 'edit' ? `编辑 Key · ${String(selectedKey!.id)}` : mode === 'add-key' ? `添加 Key · ${preset!.providerId}` : '添加供应商';
  const newFound = discovery ? discovery.models.filter(model => !modelIds.includes(model)) : [];
  // Everything worth offering: what is ticked, what the Key is authorised for now, what was found.
  const offered = [...new Set([...modelIds, ...seen, ...(savedModels ?? []), ...(discovery?.models ?? [])])];
  const needle = modelQuery.trim().toLowerCase();
  const visible = needle ? offered.filter(model => model.toLowerCase().includes(needle)) : offered;
  const setTicked = (next: string[]) => {setDirty(true); setSeen(offered); setModels([...new Set(next)].join('\n'));};
  const tick = (model: string, on: boolean) => setTicked(on ? [...modelIds, model] : modelIds.filter(item => item !== model));
  const discoverBlocked = !provider.trim() ? '填写供应商 ID 后可获取' : mode === 'provider' && !providerExists ? '先保存供应商，再获取模型列表'
    : mode === 'provider' && !secret ? '填写 API Key 后可获取' : !workingKeyId ? '填写 Key ID 后可获取' : undefined;
  const providerBlocked = review ? '先重新读取确认上次的结果' : !provider.trim() || !secret || !baseUrl.trim() ? '填写供应商 ID、上游地址和 API Key 后可保存' : undefined;
  const saveBlocked = review ? '先重新读取确认上次的结果' : !provider.trim() || !keyId.trim() ? '填写 Key ID 后可保存' : undefined;
  return <section id="key-editor" className="panel key-editor" aria-label={title}>
    <div className="panel-head">
      <h3>{title}</h3>
      {onClose && <button type="button" className="btn-icon" aria-label="关闭编辑器" title="关闭" disabled={busy} onClick={onClose}><IconClose/></button>}
    </div>
    <fieldset disabled={busy} className="form-grid form-grid-2" onChange={() => setDirty(true)}>
      {/* Changing the provider or Key of an existing Key would make another Key: both stay fixed. */}
      <label className="field"><span className="field-label">供应商 ID</span>
        <input aria-label="供应商 ID" value={provider} readOnly={mode !== 'provider'} onChange={event => setProvider(event.target.value)}/></label>
      {mode === 'provider' ? <>
        <label className="field"><span className="field-label">上游地址</span>
          <input aria-label="上游地址" type="url" value={baseUrl} placeholder="https://api.example.com" onChange={event => setBaseUrl(event.target.value)}/></label>
        <label className="field"><span className="field-label">接口格式</span>
          <select aria-label="接口格式" value={format} onChange={event => setFormat(event.target.value)}><option value="anthropic">Anthropic</option><option value="openai">OpenAI</option></select></label>
      </> : <label className="field"><span className="field-label">Key ID</span>
        <input aria-label="Key ID" value={keyId} readOnly={mode === 'edit'} onChange={event => setKeyId(event.target.value)}/></label>}
      <label className="field"><span className="field-label">API Key</span>
        <input aria-label="API Key" ref={secretInput} type="password" autoComplete="new-password" placeholder={mode === 'edit' ? '已保存，留空不修改' : '必填'}
          onChange={event => setSecret(event.target.value)}/></label>
      {mode !== 'provider' && <label className="field"><span className="field-label">权重<InfoTip text="多个 Key 时按权重分配请求（1–1000 的整数）"/></span>
        <input aria-label="权重" type="number" min={1} max={1000} step={1} value={weight} onChange={event => setWeight(event.target.value)}/></label>}
      <div className="field field-span model-picker" role="group" aria-label="可用模型">
        <div className="model-picker-head">
          <span className="field-label">可用模型</span>
          {offered.length > 8 && <input className="model-search" aria-label="搜索模型" placeholder="搜索" value={modelQuery} onChange={event => setModelQuery(event.target.value)}/>}
          <span className="button-row">
            <button type="button" className="btn-text" disabled={!visible.length} onClick={() => setTicked([...modelIds, ...visible])}>全选</button>
            <button type="button" className="btn-text" disabled={!visible.some(model => modelIds.includes(model))} onClick={() => setTicked(modelIds.filter(model => !visible.includes(model)))}>全不选</button>
            <button type="button" className="btn-text" aria-expanded={manual} onClick={() => setManual(!manual)}>{manual ? '收起手动输入' : '手动输入'}</button>
          </span>
        </div>
        {visible.length ? <ul className="model-checklist">{visible.map(model => {
          const fresh = !!discovery?.models.includes(model) && !(savedModels ?? []).includes(model);
          const unlisted = modelsKnown && modelIds.includes(model) && !knownModels.includes(model);
          // Only a saved permission can be listed: showing a model needs an enabled Key that allows it.
          const listable = unlisted && !!onListModel && mode === 'edit' && !dirty && enabled && (savedModels ?? []).includes(model);
          return <li key={model}><label className="check-field">
            <input type="checkbox" aria-label={model} checked={modelIds.includes(model)} onChange={event => tick(model, event.target.checked)}/>
            <span className="mono">{model}</span>
            {fresh && <span className="tag tag-info">新</span>}
            {unlisted && <span className="tag" title="还没在“模型与定价”上架，客户看不到">未上架</span>}
          </label>{listable && <button type="button" className="btn-text btn-small" onClick={() => onListModel!(provider.trim(), model)}>去上架</button>}
            {mode === 'edit' && <Probe providerId={provider.trim()} model={model} keyId={keyId.trim()} title="用已保存的这个 Key 发一次很小的真实请求（花费不到 1 分钱），不保存任何东西"/>}</li>;
        })}</ul> : <p className="muted">{offered.length ? '没有匹配的模型' : '还没有模型：点“获取模型列表”，或手动输入'}</p>}
        {manual && <label className="field"><span className="field-label">可用模型（每行一个）</span>
          <textarea aria-label="可用模型（每行一个）" rows={6} className="mono" value={models} onChange={event => setModels(event.target.value)}/></label>}
        {modelIds.length ? <span className="field-hint">已选 {modelIds.length} 个</span> : <span className="field-warning">未选择模型：这个 Key 不会被使用</span>}
      </div>
      {discovery && <div className="field-span discovery" aria-label="获取到的模型">
        <p>获取到 {discovery.models.length} 个模型{discovery.incomplete ? '（不完整）' : ''}{newFound.length ? `，${newFound.length} 个还不在列表中` : '，都已在列表中'}</p>
        {newFound.length > 0 && <p className="model-tags">{newFound.slice(0, 30).map(model => <span key={model} className="tag tag-info">{model}</span>)}{newFound.length > 30 && <span className="muted">+{newFound.length - 30}</span>}</p>}
        <button type="button" className="btn btn-small" disabled={busy || !discovery.models.length || !newFound.length} onClick={() => {
          setDirty(true); setModels([...new Set([...modelIds, ...discovery.models])].join('\n'));
          setMessage({tone: 'info', text: '已加入列表，原有模型保留；保存后生效'});
        }}>全部加入</button>
      </div>}
      {mode === 'provider'
        ? <p className="field-hint field-span">将创建 Key：{workingKeyId || '供应商 ID-key-1'}（启用，权重 1）</p>
        : <label className="check-field field-span"><input type="checkbox" checked={enabled} onChange={event => setEnabled(event.target.checked)}/> 启用</label>}
    </fieldset>
    <div className="editor-actions">
      {message && <p role="status" className={`message message-${message.tone}`}>{message.text}</p>}
      {busy && !message && <p role="status" className="message message-info">处理中…</p>}
      <div className="button-row">
        {mode === 'edit' && <button type="button" className="btn btn-danger" disabled={busy || !!review} title={review ? '先重新读取确认上次的结果' : undefined} onClick={() => void remove()}>删除 Key</button>}
        {review && <button type="button" className="btn" disabled={busy} onClick={() => void reviewKey()}>重新读取</button>}
        <button type="button" className="btn" disabled={busy || !!discoverBlocked} title={discoverBlocked} onClick={() => void act('discover')}>获取模型列表</button>
        {mode === 'provider'
          ? <button type="button" className="btn btn-primary" disabled={busy || !!providerBlocked} title={providerBlocked} onClick={() => void createProvider()}>保存供应商</button>
          : <button type="button" className="btn btn-primary" disabled={busy || !!saveBlocked} title={saveBlocked} onClick={() => void act('save')}>保存</button>}
      </div>
    </div>
  </section>;
}
