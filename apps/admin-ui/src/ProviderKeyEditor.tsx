import { useEffect, useRef, useState } from 'react';
import { adminApi, AdminApiError } from './api';

export default function ProviderKeyEditor({ selectedKey, onSaved, onDirtyChange, onBusyChange }: {selectedKey?: Record<string, unknown>; onSaved?: (savedKey?: Record<string, unknown>) => void; onDirtyChange: (dirty: boolean) => void; onBusyChange: (busy: boolean) => void}) {
  const [provider, setProvider] = useState('');
  const [baseUrl, setBaseUrl] = useState('');
  const [format, setFormat] = useState('anthropic');
  const [keyId, setKeyId] = useState('');
  const [secret, setSecret] = useState('');
  const [models, setModels] = useState('');
  const [weight, setWeight] = useState('1');
  const [enabled, setEnabled] = useState(true);
  const [busy, setBusy] = useState(false);
  const [dirty, setDirty] = useState(false);
  const pending = useRef(false);
  const [review, setReview] = useState<{provider: string; key: string} | null>(null);
  const modelIds = [...new Set(models.split(/\r?\n/).map(model => model.trim()).filter(Boolean))];
  const validText = (text: string, max: number) => !!text.trim() && new TextEncoder().encode(text).length <= max && !/[\x00-\x1f\x7f-\x9f]/.test(text);
  const validate = (importing = false) => {
    if (!validText(provider.trim(), 256)) throw new Error('供应商 ID 不能为空，不能包含控制字符，且不能超过 256 字节');
    if (!importing && !validText(keyId.trim(), 256)) throw new Error('Key ID 不能为空，不能包含控制字符，且不能超过 256 字节');
    if ((secret || importing) && (!validText(secret, 4096) || secret !== secret.trim())) throw new Error('API Key 不能为空，不能包含首尾空白或控制字符，且不能超过 4096 字节');
  };
  const validateModels = () => {
    if (modelIds.length > 1000 || modelIds.some(model => !validText(model, 256))) throw new Error('最多允许 1000 个模型，每个 ID 不超过 256 字节且不能包含控制字符');
  };
  const writeFailure = (error: unknown, target: {provider: string; key: string}) => {
    const rejected = error instanceof AdminApiError && [400, 401, 403, 404, 409, 413, 422].includes(error.status);
    if (!rejected) setReview(target);
    setMessage(`${error instanceof Error ? error.message : String(error)}。${rejected ? '服务端拒绝了本次操作，草稿已保留，请修正后再保存。' : '写入结果未确认，已暂停保存和导入。请重新读取 Key 核对，不要重复提交。'}`);
  };
  useEffect(() => {onDirtyChange(dirty);}, [dirty, onDirtyChange]);
  useEffect(() => {onBusyChange(busy); return () => onBusyChange(false);}, [busy, onBusyChange]);
  const [discovery, setDiscovery] = useState<{key: string; models: string[]; incomplete: boolean} | null>(null);
  useEffect(() => {
    if (!selectedKey) return;
    setProvider(String(selectedKey.provider_id ?? '')); setKeyId(String(selectedKey.id ?? ''));
    setModels(Array.isArray(selectedKey.allowed_models) ? selectedKey.allowed_models.join('\n') : '');
    setWeight(String(selectedKey.weight ?? 1)); setEnabled(selectedKey.enabled !== false); setSecret(''); setDiscovery(null);
  // Refreshes must not replace an in-progress draft; remounting reads the latest saved row.
  }, [selectedKey?.id, selectedKey?.provider_id]);
  useEffect(() => {setDiscovery(null);}, [provider, keyId, secret]);
  const [message, setMessage] = useState('模型目录仅供配置参考，保存授权后还需在“模型与定价”中发布。');
  const act = async (action: 'save' | 'discover') => {
    if (pending.current || (action === 'save' && review)) return;
    try {
      validate();
      if (action === 'save') {
        validateModels();
        if (!weight.trim() || !Number.isInteger(Number(weight)) || Number(weight) < 1 || Number(weight) > 1000) throw new Error('权重须为 1 至 1000 的整数');
        if (!window.confirm(`确认保存供应商“${provider.trim()}”的 Key“${keyId.trim()}”？\n${modelIds.length ? `允许 ${modelIds.length} 个模型` : '空模型列表将拒绝所有模型'}；权重 ${weight}；${enabled ? '启用' : '停用'}。${secret ? '\n将写入本次填写的 API Key。' : '\n保留已有 API Key；新增 Key 必须填写密钥。'}`)) return;
      }
    } catch (error) { setMessage(error instanceof Error ? error.message : String(error)); return; }
    pending.current = true; setBusy(true);
    if (action === 'discover') setDiscovery(null);
    try {
      const result = await adminApi.manageKey(action, {
        provider_id: provider.trim(), key_id: keyId.trim(), ...(secret ? {api_key: secret} : {}),
        ...(action === 'save' ? {allowed_models: modelIds, weight: Number(weight), enabled} : {}),
      });
      if (result.success !== true) throw new AdminApiError('服务器未确认操作成功', 400);
      if (action === 'discover') {
        if (!Array.isArray(result.models) || result.models.some(model => typeof model !== 'string' || !validText(model, 256))) throw new Error('候选模型格式无效，原权限草稿未更改');
        const candidates = [...new Set(result.models)];
        setDiscovery({key: keyId.trim(), models: candidates, incomplete: Boolean(result.has_more)});
        setMessage(!candidates.length ? '上游未返回候选模型，原权限草稿未更改。可手动填写已确认可用的模型 ID。' : result.has_more ? '只获取到上游第一页，列表不完整。填入时会合并现有草稿，不删除原权限；请补全后保存。' : '候选模型已读取，尚未更改权限。请核对可调用性、价格与能力后保存；用户目录仍需单独发布。');
      } else {
        setDirty(false); setSecret('');
        setMessage('密钥授权已保存。用户可用模型请在“模型与定价”中配置并发布。');
        onSaved?.({id: keyId.trim(), provider_id: provider.trim(), allowed_models: modelIds, weight: Number(weight), enabled});
      }
    } catch (error) {
      if (action === 'save') writeFailure(error, {provider: provider.trim(), key: keyId.trim()});
      else setMessage(`${error instanceof Error ? error.message : String(error)}。发现失败，原权限草稿未更改，可重新获取。`);
    } finally { pending.current = false; setBusy(false); }
  };
  const createProvider = async () => {
    if (pending.current || review) return;
    try {
      validate(true); validateModels();
      let url: URL;
      try { url = new URL(baseUrl.trim()); } catch { throw new Error('请填写完整的上游地址，例如 https://api.example.com'); }
      if (!(url.protocol === 'https:' || (url.protocol === 'http:' && ['localhost', '127.0.0.1'].includes(url.hostname)))) throw new Error('上游地址须使用 HTTPS（本机 localhost、127.0.0.1 可用 HTTP）');
      if (!window.confirm(`确认导入渠道“${provider.trim()}”？\n地址：${baseUrl.trim()}\n同名渠道会更新地址、格式，并启用渠道；默认 Key“${provider.trim()}-key-1”将更新密钥和 ${modelIds.length} 个模型权限，并重置为启用、权重 1。下方自定义 Key ID、权重及启用状态不参与导入。${modelIds.length ? '' : '\n空模型列表将拒绝所有模型。'}\n不会发布模型或价格。`)) return;
    } catch (error) { setMessage(error instanceof Error ? error.message : String(error)); return; }
    pending.current = true; setBusy(true);
    try {
      const result = await adminApi.importProvider({providers: [{id: provider.trim(), name: provider.trim(), api_type: format, base_url: baseUrl.trim(), api_key: secret, models: modelIds}]});
      if (result.success !== true) throw new AdminApiError('服务器未确认供应商已保存', 400);
      setDirty(false); setKeyId(provider.trim() + '-key-1'); setWeight('1'); setEnabled(true); setSecret(''); onSaved?.();
      setMessage('渠道已导入，默认 Key ID 已填入（启用、权重 1）。模型权限已按草稿保存；用户模型与价格仍需单独发布。');
    } catch (error) { writeFailure(error, {provider: provider.trim(), key: provider.trim() + '-key-1'}); }
    finally { pending.current = false; setBusy(false); }
  };
  const reviewKey = async () => {
    if (pending.current || !review || !window.confirm('重新读取将替换当前 Key 权限草稿并清空密钥输入。请核对返回的权限；接口不返回密钥原文，无法据此确认密钥是否已更换。继续吗？')) return;
    pending.current = true; setBusy(true);
    try {
      const result = await adminApi.getProviders();
      if (result.success !== true || !Array.isArray(result.keys)) throw new Error('Key 列表读取未确认');
      const key = result.keys.find(row => row.id === review.key && row.provider_id === review.provider);
      setProvider(review.provider); setKeyId(review.key); setSecret(''); setDiscovery(null);
      if (key) {setModels(Array.isArray(key.allowed_models) ? key.allowed_models.join('\n') : ''); setWeight(String(key.weight ?? 1)); setEnabled(key.enabled !== false);}
      setReview(null); setDirty(!key);
      if (key) onSaved?.(key);
      setMessage(key ? '已重新读取 Key 权限，请核对后编辑。密钥原文不可读取，不能据此确认是否已更换；如需验证可重新获取候选模型。' : '服务端列表未找到该 Key。原模型草稿已保留，密钥输入已清空；请核对供应商和 Key ID 后再操作。');
    } catch (error) { setMessage(`${error instanceof Error ? error.message : String(error)}。核对失败，仍禁止重复保存或导入。`); }
    finally {pending.current = false; setBusy(false);}
  };
  const cls = 'block w-full p-2 bg-white border border-[#E5E8E5] rounded';
  return <div className="two-columns page-supplement"><section id="key-editor" className="panel p-4 border border-[#E5E8E5] rounded space-y-3">
    <h3>API 密钥与模型授权</h3>
    <p className="text-sm text-[#7B8388]">先从上方列表选择 Key，再调整模型权限。相同 Key ID 为更新操作；留空密钥保留原值，手动新增时须填写密钥。发现模型、保存权限和发布用户目录是三个独立步骤。</p>
    <fieldset disabled={busy} className="field-grid" onChange={() => setDirty(true)}>
      <label className="block">供应商 ID<input className={cls} value={provider} onChange={e=>setProvider(e.target.value)} /></label>
      <details><summary>新增或更新供应商渠道</summary><div className="space-y-2 pt-2">
        <p className="muted">导入使用默认 Key ID：供应商 ID + -key-1，会启用渠道与默认 Key，并将权重重置为 1；下方自定义 Key ID、权重、启用状态不参与导入。</p>
        <label className="block">上游地址<input type="url" className={cls} value={baseUrl} onChange={e=>setBaseUrl(e.target.value)} placeholder="https://api.example.com" /></label>
        <label className="block">接口格式<select className={cls} value={format} onChange={e=>setFormat(e.target.value)}><option value="anthropic">Anthropic</option><option value="openai">OpenAI</option></select></label>
        <button className="p-2 bg-[#EFF1EF] rounded" disabled={!!review || !provider.trim() || !secret || !baseUrl.trim()} onClick={()=>void createProvider()}>使用下方密钥与模型草稿导入渠道</button>
      </div></details>
      <label className="block">Key ID<input className={cls} value={keyId} onChange={e=>setKeyId(e.target.value)} /></label>
      <label className="block">API Key（新增必填，更新可留空）<input type="password" autoComplete="new-password" className={cls} value={secret} onChange={e=>setSecret(e.target.value)} /></label>
      <label className="block">允许模型（每行一个精确 ID）<textarea rows={6} className={cls} value={models} onChange={e=>setModels(e.target.value)} /></label>
      <p className="muted">当前草稿 {modelIds.length} 个唯一模型，保存时自动去重。空列表表示拒绝所有模型，不是允许全部。</p>
      <label className="block">权重<input type="number" min={1} max={1000} step={1} className={cls} value={weight} aria-describedby="key-weight-help" onChange={e=>setWeight(e.target.value)} /></label>
      <p id="key-weight-help" className="muted">1 至 1000 的整数，用于可用 Key 之间的相对分配；不会改变模型价格。停用 Key 会停止后续选用，不会删除配置。</p>
      <label><input type="checkbox" checked={enabled} onChange={e=>setEnabled(e.target.checked)} /> 启用</label>
      <div className="flex gap-3"><button className="p-2 bg-[#EFF1EF] rounded" disabled={!provider.trim() || !keyId.trim()} onClick={()=>void act('discover')}>获取模型草稿</button><button className="p-2 bg-[#B94B39] text-white rounded" disabled={!!review || !provider.trim() || !keyId.trim()} onClick={()=>void act('save')}>确认保存权限</button></div>
    </fieldset>
    {review && <button disabled={busy} onClick={()=>void reviewKey()}>重新读取 Key 核对</button>}
    <p role="status" className="text-[#A87029] text-sm">{busy ? '处理中…' : message}</p>
  </section><section className="notice-panel"><h3>发现结果{discovery ? ` · ${discovery.key}` : ''}</h3><p className="muted">发现仅生成候选列表，不证明模型可调用，不自动修改已保存权限。填入时合并并去重，保留已有模型；不自动授权，也不自动发布。</p>{discovery ? <><p>来源 Key：{discovery.key} · {discovery.models.length} 个候选模型</p><p>{discovery.incomplete ? '只取得第一页，目录完整性尚未确认。' : '已取得上游返回的模型目录。'}</p><ul>{discovery.models.map(model => <li key={model}>{model}</li>)}</ul><div className="actions"><button className="primary" disabled={busy || !discovery.models.length} onClick={() => {setDirty(true); setModels([...new Set([...modelIds, ...discovery.models])].join('\n')); setMessage('已合并到未保存的权限草稿，原有模型已保留；请核对后确认保存。');}}>填入权限草稿</button></div></> : <p className="empty-state">选择或填写 Key，再获取候选模型。</p>}</section></div>;
}
