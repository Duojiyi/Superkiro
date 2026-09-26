// 上架模型: one drawer. Pick an upstream model an enabled Key is authorised for, say what
// customers see, price it (typed, copied from a model it resembles, or computed from the
// official price and the multipliers), then 上架. The model is published hidden together with
// its first price and shown once that price is in force (listing.ts), so a customer never sees
// a model that cannot be charged for.
import {useEffect, useRef, useState} from 'react';
import {adminApi, type CommercialConfig} from './api';
import {confirmAction} from './components/confirm';
import {IconClose} from './components/icons';
import {Drawer} from './components/modal';
import {toast} from './components/toast';
import {InfoTip} from './components/ui';
import {formatCount, formatTokenCount} from './format';
import {buildListing, costFromOfficial, creditsFromOfficial, displayNameFor, groupModels, LISTING_DELAY_SECS, showListing} from './listing';
import type {PublishOutcome} from './PriceDrawer';
import {COST_FIELDS, creditsText, currentVersion, PRICE_FIELDS, sampleCost} from './priceChange';
import {formatMicroPrice, priceToMicroPerMillion} from './pricing';
import {authorizedModels, canRoute} from './routes';
import {parseTokenInput} from './tokens';

type Row = Record<string, unknown>;
/** Where the drawer starts: the provider and upstream model to list, when known. */
export interface ListingPreset {providerId?: string; model?: string}
type Phase = 'form' | 'publishing' | 'waiting' | 'showing' | 'failed';

const RATES_KEY = 'admin-listing-rates:v1';
const blank = (fields: readonly (readonly [string, string])[]) => Object.fromEntries(fields.map(([field]) => [field, '']));
const yuan = (value: number | null) => value === null ? '—' : `¥${value < 0.01 ? value.toFixed(4) : value.toFixed(2)}`;
const validReason = (text: string) => !!text.trim() && new TextEncoder().encode(text.trim()).length <= 500 && !/[\x00-\x1f\x7f-\x9f]/.test(text);
const tokenValue = (text: string) => {const value = parseTokenInput(text); return typeof value === 'number' ? value : null;};
const OFFICIAL_LABELS = ['输入', '输出', '缓存写', '缓存读'];
// Prices start on the server's clock: the listing is timed by it, not by this computer's.
const serverNow = () => adminApi.serverNowMs / 1000;

export default function ListModelDrawer({preset, config, providers, providerKeys, onClose, onPublish, onReload}: {
  preset: ListingPreset;
  config: CommercialConfig;
  providers: Row[];
  providerKeys: Row[];
  onClose: () => void;
  onPublish: (update: {models: Row[]; versions?: Row[]}, reason: string) => Promise<PublishOutcome>;
  onReload: () => Promise<void>;
}) {
  const routable = (id: unknown) => authorizedModels(id, providerKeys).length > 0;
  const [providerId, setProviderId] = useState(() => preset.providerId
    ?? String(providers.find(provider => provider.enabled !== false && routable(provider.id))?.id ?? providers[0]?.id ?? ''));
  const [targetModel, setTargetModel] = useState(preset.model ?? '');
  const [modelIdText, setModelIdText] = useState<string | null>(null);
  const modelId = modelIdText ?? targetModel.trim();
  const [displayName, setDisplayName] = useState('');
  const [groupId, setGroupId] = useState(() => String((config.groups.find(group => group.issuance_enabled !== false) ?? config.groups[0])?.id ?? ''));
  const [reference, setReference] = useState('');
  const [after, setAfter] = useState('');
  const [contextText, setContextText] = useState('200K');
  const [outputText, setOutputText] = useState('32K');
  const [tools, setTools] = useState(true), [vision, setVision] = useState(false), [reasoning, setReasoning] = useState(false);
  const [rateMultiplier, setRateMultiplier] = useState('');
  const [prices, setPrices] = useState<Record<string, string>>(() => blank(PRICE_FIELDS));
  const [costs, setCosts] = useState<Record<string, string>>(() => blank(COST_FIELDS));
  const [currency, setCurrency] = useState('CNY');
  const [official, setOfficial] = useState(['', '', '', '']);
  const [rates, setRates] = useState<{retail: string; upstream: string}>(() => {
    try {const saved = JSON.parse(localStorage.getItem(RATES_KEY) || '{}'); return {retail: String(saved.retail ?? ''), upstream: String(saved.upstream ?? '')};}
    catch {return {retail: '', upstream: ''};}
  });
  const [reasonText, setReasonText] = useState<string | null>(null);
  const [tokens, setTokens] = useState(['1000', '1000', '0', '0']);
  const [phase, setPhase] = useState<Phase>('form');
  const [error, setError] = useState('');
  const [conflict, setConflict] = useState(false);
  const [listed, setListed] = useState<{id: string; name: string; until: number} | null>(null);
  const [, setTick] = useState(0);
  const alive = useRef(true);
  useEffect(() => () => {alive.current = false;}, []);
  // The async steps below read the latest configuration and callbacks, not those of the render they began in.
  const latest = useRef(config); latest.current = config;
  const publishRef = useRef(onPublish); publishRef.current = onPublish;
  const reloadRef = useRef(onReload); reloadRef.current = onReload;
  useEffect(() => {
    if (phase !== 'waiting') return;
    const timer = setInterval(() => setTick(value => value + 1), 1000);
    return () => clearInterval(timer);
  }, [phase]);

  const provider = providers.find(item => item.id === providerId);
  const providerName = String(provider?.name ?? providerId);
  const group = config.groups.find(item => item.id === groupId) ?? null;
  const peers = groupModels(config.models, groupId);
  const choices = authorizedModels(providerId, providerKeys);
  const listedTargets = new Set(config.models.filter(model => model.target_provider_id === providerId && model.group_id === groupId).map(model => String(model.target_model)));
  const routeWarning = targetModel.trim() && provider && !canRoute(providerId, targetModel.trim(), providerKeys)
    ? `${providerName} 的 Key 还没有授权这个模型，先在“供应商与 Key”里勾选它并保存` : '';
  const reason = reasonText ?? (modelId ? `上架 ${modelId}（${providerName}）` : '');
  const working = phase === 'publishing' || phase === 'showing';
  const settings = config.settings;

  const applyReference = (id: string) => {
    setReference(id);
    const model = config.models.find(item => item.id === id);
    if (!model) return;
    setGroupId(String(model.group_id)); setAfter(id);
    setContextText(String(model.context_window ?? '')); setOutputText(String(model.max_output ?? ''));
    setTools(model.supports_tools === true); setVision(model.supports_vision === true); setReasoning(model.supports_reasoning === true);
    setRateMultiplier(typeof model.rate_multiplier === 'number' ? String(model.rate_multiplier) : '');
    const card = config.groups.find(item => item.id === model.group_id)?.rate_card_id;
    const price = currentVersion(config.versions, card, [model.exposed_model_id, model.target_model], serverNow());
    if (price?.pricing_mode === 'fixed') setPrices(Object.fromEntries(PRICE_FIELDS.map(([field]) => [field, creditsText(price[field]) ?? ''])));
  };

  // 按官方价计算: credits from the retail multiplier, procurement from the upstream's; either may be left out.
  const compute = () => {
    setError('');
    try {
      if (official.some(value => !value.trim())) throw new Error('先填四项官方价（美元 / 百万 Tokens，免费填 0）');
      if (!rates.retail.trim() && !rates.upstream.trim()) throw new Error('填写售价倍率或成本倍率（至少一项）');
      if (rates.retail.trim()) setPrices(Object.fromEntries(PRICE_FIELDS.map(([field], index) => [field, formatMicroPrice(creditsFromOfficial(official[index], rates.retail, settings?.credit_face_value_cny))])));
      if (rates.upstream.trim()) {setCosts(Object.fromEntries(COST_FIELDS.map(([field], index) => [field, String(costFromOfficial(official[index], rates.upstream))]))); setCurrency('CNY');}
      try {localStorage.setItem(RATES_KEY, JSON.stringify(rates));} catch {/* remembered only as a convenience */}
    } catch (cause) {setError(cause instanceof Error ? cause.message : String(cause));}
  };

  let preview = '', previewError = '', losing = false;
  try {
    const result = sampleCost({rates: PRICE_FIELDS.map(([field]) => priceToMicroPerMillion((prices[field] ?? '').trim(), 'million')), tokens,
      multipliers: [1, Number(group?.margin_multiplier ?? 1), 1], faceValueCny: settings?.credit_face_value_cny,
      costs: COST_FIELDS.map(([field]) => Number((costs[field] ?? '').trim() || NaN)), currency, usdCnyRate: settings?.usd_cny_rate});
    preview = `${formatMicroPrice(result.credits)} 积分（≈ ${yuan(result.yuan)}）· 采购 ≈ ${yuan(result.costYuan)} · 毛利${result.marginPct === null ? '暂不计算' : `约 ${Math.round(result.marginPct)}%`}`;
    losing = result.marginPct !== null && result.marginPct < 0;
  } catch {previewError = '填好售价后显示示例扣费';}
  const sampleLabel = `示例 ${formatCount(Number(tokens[0]) || 0)} 输入 + ${formatCount(Number(tokens[1]) || 0)} 输出${Number(tokens[2]) ? ` + ${formatCount(Number(tokens[2]))} 缓存写` : ''}${Number(tokens[3]) ? ` + ${formatCount(Number(tokens[3]))} 缓存读` : ''}`;
  const blocked = working || phase === 'waiting' ? '正在上架' : !validReason(reason) ? '填写原因后可上架（最多约 160 字）' : undefined;

  // Step two: the model shown at its place. A configuration changed meanwhile is reread once.
  const show = async (id: string, name: string) => {
    setPhase('showing'); setError(''); setConflict(false);
    for (let attempt = 0; ; attempt++) {
      let models: Row[];
      try {models = showListing(latest.current.models, id);}
      catch (cause) {setPhase('failed'); setError(cause instanceof Error ? cause.message : String(cause)); return;}
      const outcome = await publishRef.current({models}, `显示 ${name}（价格已生效）`);
      if (!alive.current) return;
      if (outcome.ok) {toast.success(`已上架 ${name}`); onClose(); return;}
      if (outcome.conflict && attempt === 0) {await reloadRef.current(); if (!alive.current) return; continue;}
      setPhase('failed');
      setError(outcome.uncertain ? `没收到显示结果（${outcome.message}）。请重新加载，核对 ${name} 是否已对客户可见，不要重复提交。`
        : `价格已生效，但没能自动显示给客户：${outcome.message}。${name} 目前对客户隐藏；可以点“重试显示”，或在列表里勾选“客户可见”后发布。`);
      return;
    }
  };

  const submit = async () => {
    if (blocked) return;
    setError(''); setConflict(false);
    const now = serverNow(), effectiveSecs = Math.ceil(now) + LISTING_DELAY_SECS;
    let built: {mapping: Row; version: Row};
    try {
      built = buildListing({providerId, targetModel, modelId, displayName, groupId, contextWindow: tokenValue(contextText), maxOutput: tokenValue(outputText),
        tools, vision, reasoning, rateMultiplier, after, prices, costs, currency}, {config, providers, keys: providerKeys, nowSecs: now, effectiveSecs});
    } catch (cause) {setError(cause instanceof Error ? cause.message : String(cause)); return;}
    const {mapping, version} = built;
    const shownName = String(mapping.display_name ?? displayNameFor(modelId));
    const place = after ? `排在 ${String(peers.find(model => model.id === after)?.exposed_model_id ?? after)} 之后` : '排在最后';
    const capabilities = [tools && '工具', vision && '图片', reasoning && '推理'].filter(Boolean).join('、') || '无';
    const confirmed = await confirmAction({
      title: `上架 ${modelId}？`,
      facts: [
        `客户看到：${shownName}（${modelId}）· ${String(group?.name ?? groupId)} · ${place}`,
        `线路：${providerName} / ${String(mapping.target_model)}`,
        `售价：${PRICE_FIELDS.map(([field, label]) => `${label} ${creditsText(version[field]) ?? '—'}`).join(' / ')} 积分/百万`,
        `采购：${COST_FIELDS.map(([field, label]) => `${label} ${String(version[field])}`).join(' / ')} ${currency}/百万`,
        `上下文 ${formatTokenCount(mapping.context_window)} · 最大输出 ${formatTokenCount(mapping.max_output)} · 能力：${capabilities} · 显示倍率 ${typeof mapping.rate_multiplier === 'number' ? `${mapping.rate_multiplier}x` : '按价格自动换算'}`,
        `原因：${reason.trim()}`,
      ],
      consequence: `先隐藏写入价格，约 ${LISTING_DELAY_SECS} 秒后价格生效，再自动显示给客户。客户刷新模型列表或重启 Kiro 后可见。`,
      confirmLabel: '上架',
    });
    if (!confirmed || !alive.current) return;
    if (effectiveSecs <= serverNow() + 2) {setError('确认用时太久，生效时间已过，请重新点“上架”'); return;}
    setPhase('publishing');
    const outcome = await publishRef.current({models: [mapping], versions: [version]}, reason.trim());
    if (!alive.current) return;
    if (!outcome.ok) {
      if (outcome.uncertain) {onClose(); return;}
      setPhase('form'); setConflict(!!outcome.conflict);
      setError(/retroactive/i.test(outcome.message) ? '服务器拒绝了这次上架：生效时间早于服务器时间。请校准本机时间后重试。' : outcome.message);
      return;
    }
    setListed({id: String(mapping.id), name: modelId, until: effectiveSecs}); setPhase('waiting');
    await new Promise(resolve => setTimeout(resolve, Math.max(0, (effectiveSecs - serverNow()) * 1000) + 1500));
    if (alive.current) await show(String(mapping.id), modelId);
  };

  const close = async () => {
    if (working) return;
    if (phase === 'waiting' && !(await confirmAction({title: '停止自动显示？', consequence: '价格已经写入，模型会保持对客户隐藏；之后可以在列表里勾选“客户可见”后发布。', confirmLabel: '停止'}))) return;
    onClose();
  };
  const reload = async () => {
    setPhase('publishing');
    try {await reloadRef.current();} finally {if (alive.current) {setPhase('form'); setError(''); setConflict(false);}}
  };

  const left = listed ? Math.max(0, Math.ceil(listed.until - serverNow())) : 0;
  return <Drawer id="listing-drawer" label="上架模型" onClose={() => void close()} className="price-drawer listing-drawer">
    <header className="drawer-head">
      <div className="drawer-title"><span className="drawer-model">上架模型</span>{modelId && <span className="mono muted">{modelId}</span>}</div>
      <div className="drawer-tools"><button type="button" className="btn-icon" aria-label="关闭上架" title="关闭（Esc）" disabled={working} onClick={() => void close()}><IconClose/></button></div>
    </header>
    <div className="drawer-body">
      {phase === 'waiting' && <p role="status" className="note-info">已写入 {listed?.name} 的价格（对客户隐藏）。{left > 0 ? `${left} 秒后价格生效，` : '价格生效中，'}随后自动显示给客户。</p>}
      {phase === 'showing' && <p role="status" className="note-info">价格已生效，正在显示给客户…</p>}
      <fieldset disabled={phase !== 'form'} className="price-form">
        <section aria-label="线路" className="drawer-section">
          <h4>线路</h4>
          <div className="form-grid form-grid-2">
            <label className="field"><span className="field-label">供应商</span>
              <select aria-label="供应商" value={providerId} onChange={event => setProviderId(event.target.value)}>
                {!provider && <option value={providerId}>{providerId ? `${providerId}（未找到）` : '请选择'}</option>}
                {providers.map(item => <option key={String(item.id)} value={String(item.id)}>{String(item.name ?? item.id)}{item.api_type === 'openai' ? ' · OpenAI' : ''}{item.enabled === false ? '（已停用）' : ''}</option>)}
              </select></label>
            <label className="field"><span className="field-label">上游模型<InfoTip text="只能选这个供应商的 Key 已授权的模型；也可直接输入"/></span>
              <input aria-label="上游模型" list="listing-upstream-models" placeholder={choices.length ? '选择或输入' : '这个供应商还没有授权模型'} value={targetModel} onChange={event => setTargetModel(event.target.value)}/>
              <datalist id="listing-upstream-models">{choices.map(model => <option key={model} value={model}>{listedTargets.has(model) ? '已上架' : ''}</option>)}</datalist>
              {routeWarning && <span className="field-warning">{routeWarning}</span>}
              {!routeWarning && targetModel.trim() && listedTargets.has(targetModel.trim()) && <span className="field-hint">这个上游模型已经上架过；换一个模型 ID 可以再上一次</span>}
            </label>
          </div>
        </section>
        <section aria-label="客户看到的" className="drawer-section">
          <h4>客户看到的</h4>
          <label className="field"><span className="field-label">参照现有模型<InfoTip text="复制它的上下文、能力、显示倍率和当前售价，并排在它后面；采购价不复制"/></span>
            <select aria-label="参照现有模型" value={reference} onChange={event => applyReference(event.target.value)}>
              <option value="">不参照</option>
              {config.models.map(model => <option key={String(model.id)} value={String(model.id)}>{String(model.exposed_model_id)}（{String(model.target_provider_id)}）</option>)}
            </select></label>
          <div className="form-grid form-grid-2">
            <label className="field"><span className="field-label">模型 ID<InfoTip text="客户在 Kiro 里看到并请求的 ID，默认同上游模型"/></span>
              <input aria-label="模型 ID" className="mono" value={modelId} onChange={event => setModelIdText(event.target.value)}/></label>
            <label className="field"><span className="field-label">显示名称</span>
              <input aria-label="显示名称" placeholder={modelId ? `留空显示为 ${displayNameFor(modelId)}` : '留空自动生成'} value={displayName} onChange={event => setDisplayName(event.target.value)}/></label>
            <label className="field"><span className="field-label">分组</span>
              <select aria-label="分组" value={groupId} onChange={event => {setGroupId(event.target.value); setAfter('');}}>
                {config.groups.map(item => <option key={String(item.id)} value={String(item.id)}>{String(item.name ?? item.id)}</option>)}
              </select></label>
            <label className="field"><span className="field-label">位置</span>
              <select aria-label="位置" value={after} onChange={event => setAfter(event.target.value)}>
                <option value="">排在最后</option>
                {peers.map(model => <option key={String(model.id)} value={String(model.id)}>排在 {String(model.exposed_model_id)} 之后</option>)}
              </select></label>
          </div>
        </section>
        <section aria-label="能力" className="drawer-section">
          <h4>能力</h4>
          <div className="form-grid form-grid-3">
            <label className="field"><span className="field-label">上下文</span>
              <input aria-label="上下文长度" placeholder="如 200K" value={contextText} onChange={event => setContextText(event.target.value)}/>
              <span className="field-hint">{tokenValue(contextText) ? `${formatTokenCount(tokenValue(contextText))} Tokens` : '请输入正整数 Tokens'}</span></label>
            <label className="field"><span className="field-label">最大输出</span>
              <input aria-label="最大输出" placeholder="如 32K" value={outputText} onChange={event => setOutputText(event.target.value)}/>
              <span className="field-hint">{tokenValue(outputText) ? `${formatTokenCount(tokenValue(outputText))} Tokens` : '请输入正整数 Tokens'}</span></label>
            <label className="field"><span className="field-label">显示倍率<InfoTip text="只影响客户端显示，不影响扣费；留空按价格自动换算"/></span>
              <span className="input-suffix"><input aria-label="显示倍率" inputMode="decimal" placeholder="如 2.2" value={rateMultiplier} onChange={event => setRateMultiplier(event.target.value)}/><span>×</span></span></label>
          </div>
          <div className="check-row">
            <label className="check-field"><input type="checkbox" checked={tools} onChange={event => setTools(event.target.checked)}/>工具</label>
            <label className="check-field"><input type="checkbox" checked={vision} onChange={event => setVision(event.target.checked)}/>图片</label>
            <label className="check-field"><input type="checkbox" checked={reasoning} onChange={event => setReasoning(event.target.checked)}/>推理<InfoTip text="只在上游支持思考时打开，否则客户选推理强度时会报错"/></label>
          </div>
        </section>
        <section aria-label="售价" className="drawer-section">
          <h4>售价 · 积分 / 百万 Tokens</h4>
          <details className="price-advanced listing-official">
            <summary>按官方价计算</summary>
            <div className="listing-grid">
              {OFFICIAL_LABELS.map((label, index) => <label key={label} className="field"><span className="field-label">官方{label} $</span>
                <input aria-label={`官方${label}价`} inputMode="decimal" value={official[index]} onChange={event => setOfficial(values => values.map((value, i) => i === index ? event.target.value : value))}/></label>)}
              <label className="field"><span className="field-label">售价倍率<InfoTip text="客户每用官方 1 美元，花多少元（如 0.24）；按积分面值换成积分"/></span>
                <input aria-label="售价倍率" inputMode="decimal" placeholder="如 0.24" value={rates.retail} onChange={event => setRates({...rates, retail: event.target.value})}/></label>
              <label className="field"><span className="field-label">成本倍率<InfoTip text="上游每 1 美元官方价收多少元（如 0.08），算出采购价"/></span>
                <input aria-label="成本倍率" inputMode="decimal" placeholder="如 0.08" value={rates.upstream} onChange={event => setRates({...rates, upstream: event.target.value})}/></label>
            </div>
            <button type="button" className="btn btn-small" onClick={compute}>计算</button>
          </details>
          <div className="listing-grid">
            {PRICE_FIELDS.map(([field, label]) => <label key={field} className="field"><span className="field-label">{label}</span>
              <input aria-label={`${label}售价`} inputMode="decimal" value={prices[field] ?? ''} onChange={event => setPrices({...prices, [field]: event.target.value})}/></label>)}
          </div>
          <h4>采购价 · {currency} / 百万 Tokens</h4>
          <div className="listing-grid">
            {COST_FIELDS.map(([field, label]) => <label key={field} className="field"><span className="field-label">{label}</span>
              <input aria-label={`采购${label}价`} type="number" min="0" step="any" value={costs[field] ?? ''} onChange={event => setCosts({...costs, [field]: event.target.value})}/></label>)}
            <label className="field"><span className="field-label">币种</span>
              <select aria-label="采购价币种" value={currency} onChange={event => setCurrency(event.target.value)}><option value="CNY">CNY</option><option value="USD">USD</option></select></label>
          </div>
          <section className="pricing-preview" aria-label="扣费示例">
            <p role="status" className={`pricing-result${previewError ? ' pricing-result-error' : losing ? ' is-losing' : ''}`}>{previewError || <>{sampleLabel} → <strong>{preview}</strong></>}</p>
            <details className="sample-tokens"><summary>示例用量</summary>
              <div className="listing-grid">{OFFICIAL_LABELS.map((label, index) => <label key={label} className="field"><span className="field-label">{label} Tokens</span>
                <input inputMode="numeric" value={tokens[index]} onChange={event => setTokens(values => values.map((value, i) => i === index ? event.target.value : value))}/></label>)}</div>
            </details>
          </section>
        </section>
        <label className="field"><span className="field-label">原因<span className="required-mark">（必填）</span></span>
          <input aria-label="上架原因" maxLength={500} value={reason} onChange={event => setReasonText(event.target.value)}/></label>
      </fieldset>
      {error && <div role="alert" className="form-error">
        <p>{error}</p>
        {conflict && <button type="button" className="btn btn-small" onClick={() => void reload()}>重新加载</button>}
        {phase === 'failed' && listed && <button type="button" className="btn btn-small" onClick={() => void show(listed.id, listed.name)}>重试显示</button>}
      </div>}
    </div>
    <footer className="drawer-foot">
      <span className="muted">价格与模型一起发布，价格生效后才对客户显示</span>
      <span className="drawer-foot-spacer"/>
      <button type="button" className="btn" disabled={working} onClick={() => void close()}>{phase === 'failed' ? '关闭' : '取消'}</button>
      {phase !== 'failed' && <button type="button" className="btn btn-primary" disabled={!!blocked} title={blocked} onClick={() => void submit()}>{phase === 'publishing' ? '上架中…' : phase === 'waiting' || phase === 'showing' ? '等待生效…' : '上架'}</button>}
    </footer>
  </Drawer>;
}
