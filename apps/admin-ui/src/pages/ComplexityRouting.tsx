import {useEffect, useRef, useState} from 'react';
import {adminApi, AdminApiError} from '../api';
import {confirmAction} from '../components/confirm';
import {toast} from '../components/toast';
import {budgetDay, complexityLabel, decisionDistribution, decisionReason, microToYuan, modeLabel, moveProvider, newClassifier,
  routingDraft, routingError, routingProviders, routingUpdate, shutdownUpdate, validPreview, validRoutingConfig, validRoutingStatus,
  type RoutingConfig, type RoutingDecision, type RoutingDraft, type RoutingMode, type RoutingPolicy, type RoutingPreview, type RoutingStatus} from '../complexityRouting';
import {runtimeReasonError} from '../runtimeSettings';
import type {Row, WriteGuards} from '../types';

const rowName = (row: Row) => String(row.display_name || row.name || row.exposed_model_id || row.id);
const providerName = (id: string, providers: Row[]) => {
  const provider = providers.find(p => p.id === id);
  return provider ? rowName(provider) + (provider.enabled === false ? '（已停用）' : '') : `已移除上游（${id}）`;
};
const chainText = (ids: string[], providers: Row[]) => ids.length ? ids.map(id => providerName(id, providers)).join(' → ') : '无可用顺序';

function ProviderChain({label, ids, providers, onChange}: {label: string; ids: string[]; providers: Row[]; onChange: (ids: string[]) => void}) {
  return <fieldset className="routing-chain"><legend>{label}链（至少一个，按顺序尝试）</legend>
    {!ids.length && <p className="muted">尚未选择上游</p>}
    <ol>{ids.map((id, index) => <li key={index}>
      <label className="field"><span className="field-label">{label}链第 {index + 1} 上游</span>
        <select aria-label={`${label}链第 ${index + 1} 上游`} value={id} onChange={e => onChange(ids.map((value, i) => i === index ? e.target.value : value))}>
          {!providers.some(p => p.id === id) && <option value={id}>{id ? `不在当前可选目标链：${id}` : '请选择上游'}</option>}
          {providers.map(p => <option key={String(p.id)} value={String(p.id)} disabled={ids.includes(String(p.id)) && id !== p.id}>{providerName(String(p.id), providers)}</option>)}
        </select></label>
      <div className="button-row">
        <button type="button" className="btn btn-small" aria-label={`${label}链第 ${index + 1} 项上移`} disabled={index === 0} onClick={() => onChange(moveProvider(ids, index, -1))}>上移</button>
        <button type="button" className="btn btn-small" aria-label={`${label}链第 ${index + 1} 项下移`} disabled={index === ids.length - 1} onClick={() => onChange(moveProvider(ids, index, 1))}>下移</button>
        <button type="button" className="btn btn-small" aria-label={`${label}链第 ${index + 1} 项移除`} onClick={() => onChange(ids.filter((_, i) => i !== index))}>移除</button>
      </div>
    </li>)}</ol>
    <button type="button" className="btn btn-small" disabled={ids.length >= 16 || !providers.some(p => !ids.includes(String(p.id)))} onClick={() => onChange([...ids, String(providers.find(p => !ids.includes(String(p.id)))!.id)])}>添加{label}上游</button>
  </fieldset>;
}
function Decision({decision: d, providers, preview}: {decision: RoutingDecision; providers: Row[]; preview?: RoutingPreview}) {
  return <div className="routing-decision">
    <p><strong>{d.pending ? '分类处理中' : !d.classifier_attempted ? '未调用分类器' : complexityLabel[d.complexity]}</strong> · {modeLabel[d.mode]} · {d.preview ? '预览' : '请求记录'}</p>
    <p>{decisionReason(d.reason)}{d.complexity === 'unknown' && d.mode !== 'off' ? '；不确定时按复杂链处理。' : ''}</p>
    <p>分类耗时：{d.classifier_latency_ms === null ? '未提供' : `${d.classifier_latency_ms} ms`} · 运营成本：¥{microToYuan(d.classifier_cost_micro_cny)}{d.usage_estimated ? '（保守估算，可能高于实际）' : ''} · 输入 / 输出：{d.input_tokens} / {d.output_tokens} token</p>
    <p>实际服务上游：{d.served_provider_id === null ? (d.preview ? '未开始（预览不调用回答模型）' : '未确认／未开始') : providerName(d.served_provider_id, providers)}</p>
    <p>建议链：{chainText(d.provider_ids, providers)}</p>
    {preview ? <>
      <p>当前可用上游：{chainText(preview.eligible_provider_ids, providers)}</p>
      <p>{d.mode === 'observe' ? '观察模式未应用分流，仍用原顺序' : d.mode === 'off' ? '分流关闭，仍用原顺序' : '本次预览会应用的顺序'}：{chainText(preview.applied_provider_ids, providers)}</p>
    </> : <p>{d.mode === 'observe' ? '观察模式未应用分流顺序' : d.mode === 'off' ? '分流关闭，使用原路由' : '执行模式；此记录仅提供建议链，实际应用顺序请核对调用追踪'}</p>}
    <p className="muted">配置版本：{d.revision} · {new Date(d.created_at_secs * 1000).toLocaleString()}</p>
  </div>;
}

export default function ComplexityRoutingPage({models, providers, referencesLoading, referencesFailed, refreshEpoch, refreshReferences, guards, onDirtyChange, onBusyChange}: {
  models: Row[]; providers: Row[]; referencesLoading: boolean; referencesFailed: boolean; refreshEpoch: number; refreshReferences: () => Promise<void>;
  guards: WriteGuards; onDirtyChange: (value: boolean) => void; onBusyChange: (value: boolean) => void;
}) {
  const [config, setConfig] = useState<RoutingConfig | null>(null), [draft, setDraft] = useState<RoutingDraft | null>(null);
  const [status, setStatus] = useState<RoutingStatus | null>(null), [reason, setReason] = useState('');
  const [error, setError] = useState(''), [notice, setNotice] = useState(''), [statusError, setStatusError] = useState('');
  const [busy, setBusy] = useState(''), [blocked, setBlocked] = useState(false);
  const [addModel, setAddModel] = useState(''), [previewModel, setPreviewModel] = useState(''), [text, setText] = useState(''), [history, setHistory] = useState('');
  const [continuation, setContinuation] = useState(false), [attachments, setAttachments] = useState(false);
  const [preview, setPreview] = useState<RoutingPreview | null>(null), [previewError, setPreviewError] = useState('');
  const alive = useRef(true), pending = useRef(false), epoch = useRef(refreshEpoch);
  const dirty = !!config && (JSON.stringify(routingDraft(config)) !== JSON.stringify(draft) || !!reason);
  const validation = draft ? routingError(draft, models, providers) || (reason ? runtimeReasonError(reason) : '') : '';
  useEffect(() => {alive.current = true; void read(); return () => {alive.current = false;};}, []);
  useEffect(() => {onDirtyChange(dirty); return () => onDirtyChange(false);}, [dirty, onDirtyChange]);
  useEffect(() => {onBusyChange(!!busy); return () => onBusyChange(false);}, [busy, onBusyChange]);
  useEffect(() => {
    if (epoch.current === refreshEpoch) return;
    epoch.current = refreshEpoch;
    if (dirty) setNotice('全局刷新未覆盖草稿；要丢弃草稿并核对版本，请使用本页“重新读取”。');
    else void read();
  }, [refreshEpoch]);
  function start(label: string, writing = false) {
    if (pending.current || guards.writing.current) return false;
    pending.current = true; if (writing) guards.writing.current = true;
    setBusy(label); onBusyChange(true); return true;
  }
  function finish(writing = false) {
    pending.current = false; if (writing) guards.writing.current = false;
    if (alive.current) {setBusy(''); onBusyChange(false);}
  }
  function adopt(next: RoutingConfig) {
    setConfig(next); setDraft(routingDraft(next)); setReason(''); setError(''); setBlocked(false); setPreview(null); setPreviewError(''); setNotice('');
  }
  async function read(confirm = false) {
    if (!start('正在读取智能分流配置…')) return;
    try {
      if (confirm && dirty && !await confirmAction({title: '重新读取智能分流？', consequence: '将丢弃配置草稿和修改原因，读取服务器已保存版本。', confirmLabel: '重新读取'})) return;
      if (!alive.current) return;
      const result = await adminApi.getComplexityRouting();
      if (!result.success || !validRoutingConfig(result.config) || !validRoutingStatus(result.status)) throw new Error('服务器未返回有效智能分流配置或状态');
      if (alive.current) {adopt(result.config); setStatus(result.status); setStatusError('');}
    } catch (e) {if (alive.current) setError(`${e instanceof Error ? e.message : String(e)}。读取失败，已有草稿未改变，请重试。`);}
    finally {finish();}
  }
  async function refreshStatus(revision: string) {
    try {
      const result = await adminApi.getComplexityRouting();
      if (!result.success || !validRoutingStatus(result.status) || !validRoutingConfig(result.config)) throw new Error('服务器状态格式无效');
      if (alive.current) {
        setStatus(result.status); setStatusError('');
        if (result.config.revision !== revision) {setBlocked(true); setError('服务器配置已变化，草稿保留；请重新读取核对版本。');}
      }
    } catch {if (alive.current) setStatusError('最新预算与记录读取失败，当前显示可能已过期；请重新读取。');}
  }
  async function publish(shutdown = false) {
    if (!config || !draft || blocked || (!shutdown && (validation || !reason.trim() || referencesFailed || referencesLoading))) return;
    if (!start(shutdown ? '正在关停智能分流…' : '正在保存智能分流…', true)) return;
    const update = shutdown ? shutdownUpdate(config) : routingUpdate(draft, config.revision, reason);
    try {
      const enabled = update.policies.filter(p => p.mode !== 'off').length;
      if (!await confirmAction({title: shutdown ? '快速关停全部智能分流？' : '保存智能分流配置？', confirmLabel: shutdown ? '确认关停' : '确认保存', danger: shutdown,
        facts: [`基于已保存版本：${config.revision}`, shutdown ? '仅把已保存策略改为关闭；分类器、链路和普通草稿均保留。' : `发布后 ${enabled} 个模型处于观察或执行模式，其余保持关闭。保存不会自动启用。`],
        consequence: shutdown ? '新请求回到原路由。未完成请求不受影响；草稿中原有启用模式再次发布会重新启用。' : '分类器会向所选上游外发有限用户上下文。分类费用属于运营成本，不扣用户积分。价格不代表能力，请人工核查复杂请求误判。',
        option: !shutdown && update.classifier ? {label: '我确认可向所选上游外发有限用户上下文，并承担分类运营费用', required: true} : undefined})) return;
      if (!alive.current) return;
      const result = await adminApi.publishComplexityRouting(update);
      if (!result.success || !validRoutingConfig(result.config)) throw new Error('服务器未确认保存');
      if (!alive.current) return;
      if (shutdown && dirty) {
        setConfig(result.config); setError(''); setBlocked(false); setPreview(null);
        setNotice('已关停全部已保存策略；当前草稿及原因已保留，草稿中的启用模式再次发布会重新启用。');
      } else {adopt(result.config); setNotice(shutdown ? '已关停全部策略，使用原路由。' : '配置已保存，模式按草稿原样发布。');}
      toast.success(shutdown ? '智能分流已关停' : '智能分流配置已保存');
      await refreshStatus(result.config.revision);
    } catch (e) {
      if (alive.current) {
        const code = e instanceof AdminApiError ? e.status : 0;
        setBlocked(code !== 400 && code !== 503);
        const hint = code === 400 ? '配置被拒绝，草稿保留；请修正后重试。' : code === 503 ? '服务未保存，草稿保留；请稍后重试。' : code === 409 ? '版本冲突，草稿保留；必须重新读取核对版本后再保存。' : '保存结果未确认，草稿保留；请重新读取核对服务器版本，勿重复提交。';
        setError(`${e instanceof Error ? e.message : String(e)}。${hint}`);
      }
    } finally {finish(true);}
  }
  async function runPreview() {
    if (!config || dirty || blocked || !previewModel || !text.trim() || referencesLoading || referencesFailed || !start('正在预览，可能产生运营费用…', true)) return;
    try {
      if (!await confirmAction({title: '使用已保存配置预览？', confirmLabel: '确认预览', facts: [`配置版本：${config.revision}`],
        consequence: '可能向分类上游发送测试文本与有限历史上下文，产生运营费用并计入日预算；不会调用回答模型，也不扣客户积分。请勿填入敏感内容。',
        option: {label: '我确认发送测试上下文并承担本次分类费用', required: true}})) return;
      if (!alive.current) return;
      setPreview(null); setPreviewError('');
      const result = await adminApi.previewComplexityRouting({model_map_id: previewModel, text, ...(history ? {history} : {}), continuation, has_attachments: attachments});
      if (!validPreview(result)) throw new Error('服务器未返回有效预览结果');
      if (alive.current) {
        setPreview(result);
        if (result.decision.revision !== config.revision) {setBlocked(true); setError('预览使用了其他已保存版本，请重新读取核对配置。');}
      }
    } catch (e) {if (alive.current) setPreviewError(`${e instanceof Error ? e.message : String(e)}。本次可能已计入运营预算，不会自动重试；请核对预算后再试。`);}
    finally {if (alive.current) await refreshStatus(config.revision); finish(true);}
  }
  function changePolicy(index: number, patch: Partial<RoutingPolicy>) {
    if (draft) setDraft({...draft, policies: draft.policies.map((p, i) => i === index ? {...p, ...patch} : p)});
  }
  const distribution = decisionDistribution(status?.recent_decisions ?? []);
  const availableModels = models.filter(m => m.retired !== true && !draft?.policies.some(p => p.model_map_id === m.id));
  return <div className="routing-page">
    <section className="section-card" aria-labelledby="routing-title" aria-busy={!!busy}>
      <h3 id="routing-title">智能分流配置</h3>
      <p>按请求复杂度选择已配置的上游顺序。不确定走复杂链；请按能力与实测设置两条链，价格不能代表能力。</p>
      <p className="muted">已保存版本：{config?.revision ?? (busy ? '读取中' : '尚未读取')} · {dirty ? '有未保存草稿' : '无未保存修改'}</p>
      {busy && <p role="status">{busy}</p>}{error && <p role="alert" className="field-error">{error}</p>}
      {notice && <p role="status">{notice}</p>}
      {referencesLoading && <p role="status">正在读取已有模型与上游…</p>}
      {referencesFailed && <p role="alert">模型或上游读取失败，暂不能保存或预览。<button className="btn btn-small" disabled={!!busy || referencesLoading} onClick={() => void refreshReferences()}>重试模型与上游</button></p>}
      {!referencesLoading && !referencesFailed && !models.length && <p role="status">暂无已保存模型，请先到“模型与定价”添加模型。</p>}
      {!referencesLoading && !referencesFailed && !providers.length && <p role="status">暂无已有上游，请先到“供应商与 Key”配置；本页不输入或创建密钥。</p>}
      {draft && <fieldset disabled={!!busy || referencesLoading || referencesFailed} className="routing-editor">
        <legend className="sr-only">智能分流草稿</legend>
        <label className="check-field"><input type="checkbox" checked={!!draft.classifier} onChange={e => setDraft({...draft, classifier: e.target.checked ? newClassifier() : null})}/>配置分类器（不自动开启任何模型）</label>
        {!draft.classifier && <p className="muted">尚未配置分类器；可保存关闭状态，观察和执行模式需先配置分类器。</p>}
        {draft.classifier && <>
          <p id="classifier-notice">分类器使用已有上游认证，不新增密钥。会外发有限用户上下文；费用为运营费用，不扣用户积分。上下文上限不代表敏感信息脱敏。用户输入过长或上下文不完整时，直接采用复杂链或默认路由，不额外截断后分类。</p>
          <div className="form-grid form-grid-2" aria-describedby="classifier-notice">
            <label className="field"><span>分类上游</span><select aria-label="分类上游" value={draft.classifier.provider_id} onChange={e => setDraft({...draft, classifier: {...draft.classifier!, provider_id: e.target.value}})}>
              <option value="">请选择已有上游</option>
              {!!draft.classifier.provider_id && !providers.some(p => p.id === draft.classifier!.provider_id) && <option value={draft.classifier.provider_id}>已移除：{draft.classifier.provider_id}</option>}
              {providers.map(p => <option key={String(p.id)} value={String(p.id)} disabled={p.enabled === false}>{providerName(String(p.id), providers)}</option>)}
            </select></label>
            <label className="field"><span>分类器模型 ID（上游模型名）</span><input value={draft.classifier.model} autoComplete="off" onChange={e => setDraft({...draft, classifier: {...draft.classifier!, model: e.target.value}})}/></label>
            {([['timeout_ms', '分类超时（毫秒）'], ['max_input_chars', '分类上下文字符上限'], ['daily_request_limit', '每日分类调用上限'],
              ['daily_budget_yuan', '分类预算（元/日）'], ['input_price_yuan', '输入价格（元/百万 token）'], ['output_price_yuan', '输出价格（元/百万 token）']] as const).map(([key, label]) => <label className="field" key={key}><span>{label}</span>
                <input inputMode={key.endsWith('yuan') ? 'decimal' : 'numeric'} aria-describedby={key.endsWith('yuan') ? 'routing-money-hint' : undefined} value={draft.classifier![key]}
                  onChange={e => setDraft({...draft, classifier: {...draft.classifier!, [key]: e.target.value}})}/></label>)}
          </div>
          <p className="field-hint">分类超时：200–5000 毫秒；上下文上限：256–16000 字符；每日调用上限：1–100000 次，均为整数。</p>
          <p className="field-hint" id="routing-money-hint">价格请按供应商实际报价填写，不预设价格。最多 6 位小数，1 元 = 1,000,000 微元；预算按 UTC 日计算。每日预算及两项价格都须大于 0，最大 1,000,000 元（1e12 微元）。</p>
        </>}
        <h3>逐模型策略</h3><p className="muted">未添加策略的模型保持原路由；每个模型独立设置，新增策略默认关闭。双链仅可选择该模型完整目标链（主目标与后备）中的已启用上游，每链最多 16 项，不改变模型业务配置。</p>
        {!draft.policies.length && <p role="status">暂无分流策略，所有模型使用原路由。</p>}
        {draft.policies.map((policy, index) => <fieldset className="routing-policy" key={index}>
          <legend>策略 {index + 1} · {models.find(m => m.id === policy.model_map_id) ? rowName(models.find(m => m.id === policy.model_map_id)!) : `已移除模型：${policy.model_map_id}`}</legend>
          <div className="form-grid form-grid-2">
            <label className="field">策略 {index + 1} 模型<select aria-label={`策略 ${index + 1} 模型`} value={policy.model_map_id} onChange={e => changePolicy(index, {model_map_id: e.target.value})}>
              {!models.some(m => m.id === policy.model_map_id) && <option value={policy.model_map_id}>已移除模型：{policy.model_map_id}</option>}
              {models.map(m => <option key={String(m.id)} value={String(m.id)} disabled={m.retired === true || draft.policies.some((p, i) => i !== index && p.model_map_id === m.id)}>{rowName(m)} · {String(m.id)}</option>)}
            </select></label>
            <label className="field">策略 {index + 1} 模式<select aria-label={`策略 ${index + 1} 模式`} value={policy.mode} onChange={e => changePolicy(index, {mode: e.target.value as RoutingMode})}>
              {Object.entries(modeLabel).map(([value, label]) => <option key={value} value={value}>{label}</option>)}
            </select></label>
          </div>
          <div className="form-grid form-grid-2">
            <ProviderChain label="简单" ids={policy.simple_provider_ids} providers={routingProviders(models.find(m => m.id === policy.model_map_id), providers)} onChange={ids => changePolicy(index, {simple_provider_ids: ids})}/>
            <ProviderChain label="复杂" ids={policy.complex_provider_ids} providers={routingProviders(models.find(m => m.id === policy.model_map_id), providers)} onChange={ids => changePolicy(index, {complex_provider_ids: ids})}/>
          </div>
          <button className="btn btn-small" type="button" onClick={() => setDraft({...draft, policies: draft.policies.filter((_, i) => i !== index)})}>移除策略 {index + 1}</button>
        </fieldset>)}
        <div className="button-row"><label className="field">待添加模型<select aria-label="待添加模型" value={addModel} onChange={e => setAddModel(e.target.value)}><option value="">请选择模型</option>{availableModels.map(m => <option key={String(m.id)} value={String(m.id)}>{rowName(m)} · {String(m.id)}</option>)}</select></label>
          <button type="button" className="btn" disabled={draft.policies.length >= 256 || !availableModels.some(m => m.id === addModel)} onClick={() => {setDraft({...draft, policies: [...draft.policies, {model_map_id: addModel, mode: 'off', simple_provider_ids: [], complex_provider_ids: []}]}); setAddModel('');}}>添加模型策略</button></div>
        <label className="field">修改原因<input maxLength={1024} value={reason} aria-describedby="routing-reason-hint" onChange={e => setReason(e.target.value)}/></label>
        <span id="routing-reason-hint" className="field-hint">保存必填，最多 1024 UTF-8 字节，不可含控制字符。</span>
      </fieldset>}
      {!referencesLoading && !referencesFailed && validation && <p role="alert" className="field-error">{validation}</p>}
      <div className="button-row">
        <button className="btn" disabled={!!busy} onClick={() => void read(true)}>重新读取</button>
        <button className="btn btn-primary" disabled={!!busy || blocked || !dirty || !draft || !!validation || !reason.trim() || referencesFailed || referencesLoading} onClick={() => void publish()}>保存智能分流</button>
        <button className="btn btn-danger" disabled={!!busy || blocked || !config?.policies.some(p => p.mode !== 'off')} onClick={() => void publish(true)}>快速关停全部</button>
      </div>
      {config && <details><summary>最近修改记录（{config.audit.length}）</summary>{config.audit.length ? <ul>{[...config.audit].reverse().map((a, i) => <li key={i}>{new Date(a.created_at_secs * 1000).toLocaleString()} · {a.reason} · {a.previous_revision} → {a.revision}</li>)}</ul> : <p>暂无修改记录。</p>}</details>}
    </section>
    <section className="section-card" aria-labelledby="routing-preview-title">
      <h3 id="routing-preview-title">已保存配置预览</h3>
      <p>可能调用分类 API，费用计入运营日预算。仅预览，不调用回答模型，不扣客户积分。请仅使用无敏感信息的测试文本。</p>
      {dirty && <p role="status">有草稿修改，请先保存配置再预览，或重新读取以丢弃草稿。</p>}
      <fieldset disabled={!!busy || !config || referencesLoading || referencesFailed} className="routing-preview-form">
        <label className="field">预览模型<select aria-label="预览模型" value={previewModel} onChange={e => {setPreviewModel(e.target.value); setPreview(null);}}><option value="">请选择已保存模型</option>{models.map(m => <option key={String(m.id)} value={String(m.id)}>{rowName(m)} · {String(m.id)}</option>)}</select></label>
        <label className="field">测试文本<textarea aria-label="测试文本" rows={3} value={text} onChange={e => {setText(e.target.value); setPreview(null);}}/></label>
        <label className="field">历史上下文（可选）<textarea aria-label="历史上下文（可选）" rows={2} value={history} onChange={e => {setHistory(e.target.value); setPreview(null);}}/></label>
        <div className="button-row"><label className="check-field"><input type="checkbox" checked={continuation} onChange={e => {setContinuation(e.target.checked); setPreview(null);}}/>续写请求</label>
          <label className="check-field"><input type="checkbox" checked={attachments} onChange={e => {setAttachments(e.target.checked); setPreview(null);}}/>含附件（仅标记，不上传附件）</label></div>
        <button className="btn" disabled={dirty || blocked || !previewModel || !text.trim()} onClick={() => void runPreview()}>预览已保存配置</button>
      </fieldset>
      {previewError && <p role="alert" className="field-error">{previewError}</p>}
      {preview ? <div role="status"><Decision decision={preview.decision} providers={providers} preview={preview}/></div> : <p className="muted">尚无预览结果。</p>}
    </section>
    <section className="section-card" aria-labelledby="routing-status-title">
      <h3 id="routing-status-title">运营预算与最近分类</h3>
      {statusError && <p role="alert">{statusError}</p>}
      {!status ? <p role="status">{busy ? '正在读取状态…' : '状态尚不可用，请重新读取。'}</p> : <>
        <p>预算日期：{budgetDay(status.budget.day)} · 已调用 {status.budget.calls} 次 · 已用 ¥{microToYuan(status.budget.cost_micro_cny)}
          {config?.classifier ? ` / ¥${microToYuan(config.classifier.daily_budget_micro_cny)}，每日上限 ${config.classifier.daily_request_limit} 次` : '（尚未配置分类预算）'}</p>
        {status.retained_decisions !== undefined && <p>保留记录：{status.retained_decisions} / 20000 条（7 天窗口，包含预览与未调用分类器的决策）。</p>}
        {(status.retained_decisions ?? 0) >= 16000 && <p role="status">{(status.retained_decisions ?? 0) >= 20000 ? '记录容量已满：暂停新增分类调用和明细记录，按当前合法渠道继续答复；观察模式不改变原路由。' : '记录容量接近上限；达到上限后会暂停新增分类调用，保留已有幂等记录。'}</p>}
        <p>最近正式请求的分类分布：简单 {distribution.simple} · 复杂 {distribution.complex} · 不确定 {distribution.unknown}。不含预览、未调用分类器和处理中记录。</p>
        <p className="muted">这不是准确率。复杂请求误判需要人工抽查，不能由标签比例推断分类质量。</p>
        {!status.recent_decisions.length ? <p>暂无分类记录。</p> : status.recent_decisions.map((d, i) => <details key={`${d.invocation_id}-${i}`}><summary>{models.find(m => m.id === d.model_map_id) ? rowName(models.find(m => m.id === d.model_map_id)!) : d.model_map_id} · {d.preview ? '预览' : '正式请求'} · {d.pending ? '处理中' : complexityLabel[d.complexity]} · {modeLabel[d.mode]}</summary><Decision decision={d} providers={providers}/></details>)}
      </>}
    </section>
  </div>;
}
