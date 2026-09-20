import { useEffect, useState } from 'react';
import { adminApi } from './api';

export default function ProviderKeyEditor({ selectedKey, onSaved, onDirtyChange, onBusyChange }: {selectedKey?: Record<string, unknown>; onSaved?: (savedKey?: Record<string, unknown>) => void; onDirtyChange: (dirty: boolean) => void; onBusyChange: (busy: boolean) => void}) {
  const [provider, setProvider] = useState('');
  const [baseUrl, setBaseUrl] = useState('');
  const [format, setFormat] = useState('anthropic');
  const [keyId, setKeyId] = useState('');
  const [secret, setSecret] = useState('');
  const [models, setModels] = useState('');
  const [weight, setWeight] = useState(1);
  const [enabled, setEnabled] = useState(true);
  const [busy, setBusy] = useState(false);
  const [dirty, setDirty] = useState(false);
  useEffect(() => {onDirtyChange(dirty);}, [dirty, onDirtyChange]);
  useEffect(() => {onBusyChange(busy); return () => onBusyChange(false);}, [busy, onBusyChange]);
  const [discovery, setDiscovery] = useState<{key: string; models: string[]; incomplete: boolean} | null>(null);
  useEffect(() => {
    if (!selectedKey) return;
    setProvider(String(selectedKey.provider_id ?? '')); setKeyId(String(selectedKey.id ?? ''));
    setModels(Array.isArray(selectedKey.allowed_models) ? selectedKey.allowed_models.join('\n') : '');
    setWeight(Number(selectedKey.weight ?? 1)); setEnabled(selectedKey.enabled !== false); setSecret(''); setDiscovery(null);
  // Refreshes must not replace an in-progress draft; remounting reads the latest saved row.
  }, [selectedKey?.id, selectedKey?.provider_id]);
  useEffect(() => {setDiscovery(null);}, [provider, keyId]);
  const [message, setMessage] = useState('模型目录仅供配置参考，保存授权后还需在“模型与定价”中发布。');
  const act = async (action: 'save' | 'discover') => {
    if (busy || !provider.trim() || !keyId.trim()) return;
    if (action === 'save' && !window.confirm('确认替换该 Key 的模型权限、权重和启用状态？空模型列表将拒绝所有模型。')) return;
    setBusy(true);
    try {
      const result = await adminApi.manageKey(action, {
        provider_id: provider.trim(), key_id: keyId.trim(), ...(secret ? {api_key: secret} : {}),
        ...(action === 'save' ? {allowed_models: models.split(/\r?\n/).map(s=>s.trim()).filter(Boolean), weight, enabled} : {}),
      });
      if (!result.success) throw new Error('服务器未确认操作成功，请刷新核对后重试。');
      if (action === 'discover') {
        setDiscovery({key: keyId.trim(), models: result.models || [], incomplete: Boolean(result.has_more)});
        setMessage(result.has_more ? '只获取到上游第一页，列表不完整。请补全并核对后保存，不要直接覆盖原权限。' : '候选模型已读取，尚未更改权限。请核对可调用性、价格与能力后保存；用户目录仍需单独发布。');
      } else { setDirty(false); setSecret(''); onSaved?.({id: keyId.trim(), provider_id: provider.trim(), allowed_models: models.split(/\r?\n/).map(s=>s.trim()).filter(Boolean), weight, enabled}); setMessage('密钥授权已保存。用户可用模型请在“模型与定价”中配置并发布。'); }
    } catch (e) { setMessage(String(e)); } finally { setBusy(false); }
  };
  const createProvider = async () => {
    if (busy || !provider.trim() || !secret || !baseUrl.trim()) return;
    if (!window.confirm('确认导入渠道？同名渠道会更新地址、格式及默认 Key。不会发布模型或价格。')) return;
    setBusy(true);
    try {
      const result = await adminApi.importProvider({providers: [{id: provider.trim(), name: provider.trim(), api_type: format, base_url: baseUrl.trim(), api_key: secret, models: models.split(/\r?\n/).map(s=>s.trim()).filter(Boolean)}]});
      if (!result.success) throw new Error('服务器未确认供应商已保存，请刷新核对。');
      setDirty(false); setKeyId(provider.trim() + '-key-1'); setSecret(''); onSaved?.();
      setMessage('渠道已导入，默认 Key ID 已填入。可获取候选模型并保存权限；请刷新页面查看渠道列表。');
    } catch (e) { setMessage(String(e)); } finally { setBusy(false); }
  };
  const cls = 'block w-full p-2 bg-white border border-[#E5E8E5] rounded';
  return <div className="two-columns page-supplement"><section id="key-editor" className="panel p-4 border border-[#E5E8E5] rounded space-y-3">
    <h3>API 密钥与模型授权</h3>
    <p className="text-sm text-[#7B8388]">相同 Key ID 为更新操作；留空密钥保留原值。请从上方列表核对现有权限后再编辑。</p>
    <fieldset disabled={busy} className="field-grid" onChange={() => setDirty(true)}>
      <label className="block">供应商 ID<input className={cls} value={provider} onChange={e=>setProvider(e.target.value)} /></label>
      <details><summary>新增或更新供应商渠道</summary><div className="space-y-2 pt-2">
        <label className="block">上游地址<input type="url" className={cls} value={baseUrl} onChange={e=>setBaseUrl(e.target.value)} placeholder="https://api.example.com" /></label>
        <label className="block">接口格式<select className={cls} value={format} onChange={e=>setFormat(e.target.value)}><option value="anthropic">Anthropic</option><option value="openai">OpenAI</option></select></label>
        <button className="p-2 bg-[#EFF1EF] rounded" disabled={!provider.trim() || !secret || !baseUrl.trim()} onClick={()=>void createProvider()}>使用下方密钥与模型草稿导入渠道</button>
      </div></details>
      <label className="block">Key ID<input className={cls} value={keyId} onChange={e=>setKeyId(e.target.value)} /></label>
      <label className="block">API Key（新增必填，更新可留空）<input type="password" autoComplete="new-password" className={cls} value={secret} onChange={e=>setSecret(e.target.value)} /></label>
      <label className="block">允许模型（每行一个精确 ID）<textarea rows={6} className={cls} value={models} onChange={e=>setModels(e.target.value)} /></label>
      <label className="block">权重<input type="number" min={1} max={1000} className={cls} value={weight} onChange={e=>setWeight(Number(e.target.value))} /></label>
      <label><input type="checkbox" checked={enabled} onChange={e=>setEnabled(e.target.checked)} /> 启用</label>
      <div className="flex gap-3"><button className="p-2 bg-[#EFF1EF] rounded" disabled={!provider.trim() || !keyId.trim()} onClick={()=>void act('discover')}>获取模型草稿</button><button className="p-2 bg-[#B94B39] text-white rounded" disabled={!provider.trim() || !keyId.trim() || !Number.isInteger(weight) || weight < 1 || weight > 1000} onClick={()=>void act('save')}>确认保存权限</button></div>
    </fieldset>
    <p role="status" className="text-[#A87029] text-sm">{busy ? '处理中…' : message}</p>
  </section><section className="notice-panel"><h3>发现结果{discovery ? ` · ${discovery.key}` : ''}</h3><p className="muted">发现仅生成候选列表，不证明模型可调用，不自动修改已保存权限。</p>{discovery ? <><p>来源 Key：{discovery.key} · {discovery.models.length} 个候选模型</p><p>{discovery.incomplete ? '只取得第一页，目录完整性尚未确认。' : '已取得上游返回的模型目录。'}</p><ul>{discovery.models.map(model => <li key={model}>{model}</li>)}</ul><div className="actions"><button className="primary" disabled={busy} onClick={() => {setDirty(true); setModels(discovery.models.join('\n')); setMessage('已加入未保存的权限草稿，请核对后确认保存。');}}>填入权限草稿</button></div></> : <p className="empty-state">选择或填写 Key，再获取候选模型。</p>}</section></div>;
}
