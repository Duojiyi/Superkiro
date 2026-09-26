// 上架模型: one drawer. Pick an upstream model an enabled Key is authorised for (and 测试 it), say
// what customers see and where it goes in the list, price it (typed, copied from a model it
// resembles, or computed from the official price and the multipliers), then 上架. The model is
// published shown, together with its first price in force at once (listing.ts), so a customer
// never sees a model that cannot be charged for.
import {useEffect, useRef, useState} from 'react';
import {adminApi, type CommercialConfig} from './api';
import {confirmAction} from './components/confirm';
import {IconClose} from './components/icons';
import {Drawer} from './components/modal';
import {toast} from './components/toast';
import {InfoTip} from './components/ui';
import {formatCount, formatFullDateTime, formatTokenCount} from './format';
import {buildListing, costFromOfficial, creditsFromOfficial, displayNameFor, groupModels, MODEL_ID, MODEL_ID_RULE, sharedPrice, type ListingInput} from './listing';
import type {PublishOutcome} from './PriceDrawer';
import {COST_FIELDS, creditsText, currentVersion, PRICE_FIELDS, sampleCost} from './priceChange';
import {formatMicroPrice, priceToMicroPerMillion} from './pricing';
import Probe from './Probe';
import {loadOfficial, loadRates, saveOfficial, saveRates} from './remembered';
import {authorizedModels, canRoute} from './routes';
import {parseTokenInput} from './tokens';

type Row = Record<string, unknown>;
/** Where the drawer starts: the provider and upstream model to list, when known. */
export interface ListingPreset {providerId?: string; model?: string}

const FIRST = '#first';
const blank = (fields: readonly (readonly [string, string])[]) => Object.fromEntries(fields.map(([field]) => [field, '']));
const yuan = (value: number | null) => value === null ? '—' : `¥${value < 0.01 ? value.toFixed(4) : value.toFixed(2)}`;
const validReason = (text: string) => !!text.trim() && new TextEncoder().encode(text.trim()).length <= 500 && !/[\x00-\x1f\x7f-\x9f]/.test(text);
const tokenValue = (text: string) => {const value = parseTokenInput(text); return typeof value === 'number' ? value : null;};
const OFFICIAL_LABELS = ['输入', '输出', '缓存写', '缓存读'];
// Prices start on the server's clock: they are timed by it, not by this computer's.
const serverNow = () => adminApi.serverNowMs / 1000;
// A new price for a model ID its table already prices cannot start now: five minutes on.
const soon = () => Math.ceil((serverNow() + 5 * 60) / 60) * 60;
const priceSummary = (version: Row | null) => version?.pricing_mode === 'fixed'
  ? `${PRICE_FIELDS.map(([field, label]) => `${label} ${creditsText(version[field]) ?? '—'}`).join(' / ')} 积分/百万` : version ? '非固定价格' : '—';

export default function ListModelDrawer({preset, config, providers, providerKeys, onClose, onPublish, onReload}: {
  preset: ListingPreset;
  config: CommercialConfig;
  providers: Row[];
  providerKeys: Row[];
  onClose: () => void;
  /** `check`: what to look for if the result does not arrive. */
  onPublish: (update: {models: Row[]; versions?: Row[]}, reason: string, check: string) => Promise<PublishOutcome>;
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
  // '' is last; FIRST, first; otherwise the entry it follows.
  const [place, setPlace] = useState('');
  const [contextText, setContextText] = useState('200K');
  const [outputText, setOutputText] = useState('32K');
  const [tools, setTools] = useState(true), [vision, setVision] = useState(false), [reasoning, setReasoning] = useState(false);
  const [rateMultiplier, setRateMultiplier] = useState('');
  const [creditMultiplier, setCreditMultiplier] = useState('1');
  const [prices, setPrices] = useState<Record<string, string>>(() => blank(PRICE_FIELDS));
  const [costs, setCosts] = useState<Record<string, string>>(() => blank(COST_FIELDS));
  const [currency, setCurrency] = useState('CNY');
  const [official, setOfficial] = useState(() => loadOfficial(preset.model ?? ''));
  // The retail multiplier is one rule for the business; each upstream has its own cost multiplier.
  const [rates, setRates] = useState(loadRates);
  const upstreamRate = rates.upstream[providerId] ?? '';
  // When the table already prices this model ID, the existing price is kept unless the owner asks for a new one.
  const [newPrice, setNewPrice] = useState(false);
  const [reasonText, setReasonText] = useState<string | null>(null);
  const [tokens, setTokens] = useState(['1000', '1000', '0', '0']);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState('');
  const [conflict, setConflict] = useState(false);
  const alive = useRef(true);
  useEffect(() => () => {alive.current = false;}, []);
  // The steps after the confirmation read the latest configuration and callbacks, not those of the render they began in.
  const latest = useRef(config); latest.current = config;
  const publishRef = useRef(onPublish); publishRef.current = onPublish;
  const reloadRef = useRef(onReload); reloadRef.current = onReload;

  const provider = providers.find(item => item.id === providerId);
  const providerName = String(provider?.name ?? providerId);
  const group = config.groups.find(item => item.id === groupId) ?? null;
  const groupName = String(group?.name ?? groupId);
  const peers = groupModels(config.models, groupId);
  const choices = authorizedModels(providerId, providerKeys);
  const listedTargets = new Set(config.models.filter(model => model.target_provider_id === providerId && model.group_id === groupId).map(model => String(model.target_model)));
  const routeWarning = targetModel.trim() && provider && !canRoute(providerId, targetModel.trim(), providerKeys)
    ? `${providerName} 的 Key 还没有授权这个模型，先在“供应商与 Key”里勾选它并保存` : '';
  const idProblem = modelId && !MODEL_ID.test(modelId) ? MODEL_ID_RULE : '';
  const shared = modelId && !idProblem ? sharedPrice(config, groupId, modelId) : {versions: [], groups: []};
  const keepPrice = shared.versions.length > 0 && !newPrice;
  const existing = shared.versions.length ? currentVersion(config.versions, group?.rate_card_id, [modelId], serverNow()) : null;
  const sharedNames = shared.groups.map(item => String(item.name ?? item.id)).join('、');
  const reason = reasonText ?? (modelId ? `上架 ${modelId}（${providerName}）` : '');
  const settings = config.settings;

  const applyReference = (id: string) => {
    setReference(id);
    const model = config.models.find(item => item.id === id);
    if (!model) return;
    setGroupId(String(model.group_id)); setPlace(id);
    setContextText(String(model.context_window ?? '')); setOutputText(String(model.max_output ?? ''));
    setTools(model.supports_tools === true); setVision(model.supports_vision === true); setReasoning(model.supports_reasoning === true);
    setRateMultiplier(typeof model.rate_multiplier === 'number' ? String(model.rate_multiplier) : '');
    setCreditMultiplier(String(model.credit_multiplier ?? 1));
    const card = config.groups.find(item => item.id === model.group_id)?.rate_card_id;
    const price = currentVersion(config.versions, card, [model.exposed_model_id, model.target_model], serverNow());
    if (price?.pricing_mode === 'fixed') setPrices(Object.fromEntries(PRICE_FIELDS.map(([field]) => [field, creditsText(price[field]) ?? ''])));
  };

  // 按官方价计算: credits from the retail multiplier, procurement from this upstream's; either may be left out.
  const compute = () => {
    setError('');
    try {
      if (official.some(value => !value.trim())) throw new Error('先填四项官方价（美元 / 百万 Tokens，免费填 0）');
      if (!rates.retail.trim() && !upstreamRate.trim()) throw new Error('填写售价倍率或成本倍率（至少一项）');
      if (rates.retail.trim()) setPrices(Object.fromEntries(PRICE_FIELDS.map(([field], index) => [field, formatMicroPrice(creditsFromOfficial(official[index], rates.retail, settings?.credit_face_value_cny))])));
      if (upstreamRate.trim()) {setCosts(Object.fromEntries(COST_FIELDS.map(([field], index) => [field, String(costFromOfficial(official[index], upstreamRate))]))); setCurrency('CNY');}
      saveRates(rates); if (modelId) saveOfficial(modelId, official);
    } catch (cause) {setError(cause instanceof Error ? cause.message : String(cause));}
  };

  let preview = '', previewError = '', losing = false;
  try {
    const result = sampleCost({rates: PRICE_FIELDS.map(([field]) => priceToMicroPerMillion((prices[field] ?? '').trim(), 'million')), tokens,
      multipliers: [1, Number(group?.margin_multiplier ?? 1), Number(creditMultiplier) || 1], faceValueCny: settings?.credit_face_value_cny,
      costs: COST_FIELDS.map(([field]) => Number((costs[field] ?? '').trim() || NaN)), currency, usdCnyRate: settings?.usd_cny_rate});
    preview = `${formatMicroPrice(result.credits)} 积分（≈ ${yuan(result.yuan)}）· 采购 ≈ ${yuan(result.costYuan)} · 毛利${result.marginPct === null ? '暂不计算' : `约 ${Math.round(result.marginPct)}%`}`;
    losing = result.marginPct !== null && result.marginPct < 0;
  } catch {previewError = '填好售价后显示示例扣费';}
  const sampleLabel = `示例 ${formatCount(Number(tokens[0]) || 0)} 输入 + ${formatCount(Number(tokens[1]) || 0)} 输出${Number(tokens[2]) ? ` + ${formatCount(Number(tokens[2]))} 缓存写` : ''}${Number(tokens[3]) ? ` + ${formatCount(Number(tokens[3]))} 缓存读` : ''}`;
  const blocked = working ? '正在上架' : idProblem || (!validReason(reason) ? '填写原因后可上架（最多约 160 字）' : undefined);

  const input = (): ListingInput => ({providerId, targetModel, modelId, displayName, groupId, contextWindow: tokenValue(contextText), maxOutput: tokenValue(outputText),
    tools, vision, reasoning, rateMultiplier, creditMultiplier, keepPrice, prices, costs, currency,
    place: place === FIRST ? {at: 'first'} : place ? {at: 'after', id: place} : {at: 'last'}});

  const submit = async () => {
    if (blocked) return;
    setError(''); setConflict(false);
    let preview: ReturnType<typeof buildListing>;
    try {preview = buildListing(input(), {config, providers, keys: providerKeys, nowSecs: serverNow(), effectiveSecs: soon()});}
    catch (cause) {setError(cause instanceof Error ? cause.message : String(cause)); return;}
    const {mapping, version, sharedWith} = preview;
    const shownName = String(mapping.display_name ?? displayNameFor(modelId));
    const placeText = place === FIRST ? '排在最前（成为这个分组的默认模型）' : place ? `排在 ${String(peers.find(model => model.id === place)?.exposed_model_id ?? place)} 之后` : '排在最后';
    const capabilities = [tools && '工具', vision && '图片', reasoning && '推理'].filter(Boolean).join('、') || '无';
    const confirmed = await confirmAction({
      title: `上架 ${modelId}？`,
      facts: [
        `客户看到：${shownName}（${modelId}）· ${groupName} · ${placeText}`,
        `线路：${providerName} / ${String(mapping.target_model)}`,
        version ? `售价：${priceSummary(version)} · ${version.effective_from_secs === 0 ? '上架即生效' : `${formatFullDateTime(Number(version.effective_from_secs))} 起，在那之前按现有价格`}`
          : `售价：沿用价格表里现有的价格（${priceSummary(existing)}）`,
        ...(version ? [`采购：${COST_FIELDS.map(([field, label]) => `${label} ${String(version[field])}`).join(' / ')} ${currency}/百万`] : []),
        ...(version && sharedWith.length ? [`新价格同时用于 ${sharedWith.map(item => String(item.name ?? item.id)).join('、')} 分组的 ${modelId}（同一价格表）`] : []),
        `上下文 ${formatTokenCount(mapping.context_window)} · 最大输出 ${formatTokenCount(mapping.max_output)} · 能力：${capabilities} · 扣费倍率 ${String(mapping.credit_multiplier)} · 显示倍率 ${typeof mapping.rate_multiplier === 'number' ? `${mapping.rate_multiplier}x` : '按价格自动换算'}`,
        `原因：${reason.trim()}`,
      ],
      consequence: '发布后立即对客户显示：客户刷新模型列表或重启 Kiro 后可见，新请求马上可以用。',
      confirmLabel: '上架',
    });
    if (!confirmed || !alive.current) return;
    // Built again on the configuration as it is now: its place and IDs are the latest ones.
    let listing: ReturnType<typeof buildListing>;
    try {listing = buildListing(input(), {config: latest.current, providers, keys: providerKeys, nowSecs: serverNow(), effectiveSecs: soon()});}
    catch (cause) {setError(cause instanceof Error ? cause.message : String(cause)); return;}
    if (!!listing.version !== !!version || (listing.version?.effective_from_secs === 0) !== (version?.effective_from_secs === 0)) {
      setError('价格表刚有变化，这个模型 ID 的价格情况和确认时不同：请核对后重新点“上架”'); return;
    }
    setWorking(true);
    const outcome = await publishRef.current({models: listing.models, ...(listing.version ? {versions: [listing.version]} : {})}, reason.trim(),
      `“模型与定价”里有没有 ${modelId}（${groupName}）、“价格版本”里有没有它的价格`);
    if (!alive.current) return;
    setWorking(false);
    if (outcome.ok) {toast.success(`已上架 ${modelId}`); onClose(); return;}
    if (outcome.uncertain) {onClose(); return;}
    setConflict(!!outcome.conflict); setError(outcome.message);
  };

  const close = () => {if (!working) onClose();};
  const reload = async () => {
    setWorking(true);
    try {await reloadRef.current();} finally {if (alive.current) {setWorking(false); setError(''); setConflict(false);}}
  };

  return <Drawer id="listing-drawer" label="上架模型" onClose={close} className="price-drawer listing-drawer">
    <header className="drawer-head">
      <div className="drawer-title"><span className="drawer-model">上架模型</span>{modelId && <span className="mono muted">{modelId}</span>}</div>
      <div className="drawer-tools"><button type="button" className="btn-icon" aria-label="关闭上架" title="关闭（Esc）" disabled={working} onClick={close}><IconClose/></button></div>
    </header>
    <div className="drawer-body">
      <fieldset disabled={working} className="price-form">
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
            <div className="field-span probe-row"><Probe providerId={providerId} model={targetModel.trim()} disabled={!!routeWarning || provider?.enabled === false}
              title="上架前先发一次很小的真实请求（花费不到 1 分钱），看这条线路能不能用；不保存任何东西"/></div>
          </div>
        </section>
        <section aria-label="客户看到的" className="drawer-section">
          <h4>客户看到的</h4>
          <label className="field"><span className="field-label">参照现有模型<InfoTip text="复制它的上下文、能力、扣费倍率、显示倍率和当前售价，并排在它后面；采购价不复制"/></span>
            <select aria-label="参照现有模型" value={reference} onChange={event => applyReference(event.target.value)}>
              <option value="">不参照</option>
              {config.models.map(model => <option key={String(model.id)} value={String(model.id)}>{String(model.exposed_model_id)}（{String(model.target_provider_id)}）</option>)}
            </select></label>
          <div className="form-grid form-grid-2">
            <label className="field"><span className="field-label">模型 ID<InfoTip text="客户在 Kiro 里看到并请求的 ID，默认同上游模型"/></span>
              <input aria-label="模型 ID" className="mono" aria-invalid={!!idProblem} value={modelId} onChange={event => setModelIdText(event.target.value)}/>
              {idProblem && <span className="field-warning">{idProblem}</span>}</label>
            <label className="field"><span className="field-label">显示名称</span>
              <input aria-label="显示名称" placeholder={modelId ? `留空显示为 ${displayNameFor(modelId)}` : '留空自动生成'} value={displayName} onChange={event => setDisplayName(event.target.value)}/></label>
            <label className="field"><span className="field-label">分组</span>
              <select aria-label="分组" value={groupId} onChange={event => {setGroupId(event.target.value); setPlace('');}}>
                {config.groups.map(item => <option key={String(item.id)} value={String(item.id)}>{String(item.name ?? item.id)}</option>)}
              </select></label>
            <label className="field"><span className="field-label">位置<InfoTip text="排在最前的模型是 Kiro 的默认模型：客户没指定模型时用它"/></span>
              <select aria-label="位置" value={place} onChange={event => setPlace(event.target.value)}>
                <option value="">排在最后</option>
                <option value={FIRST}>排在最前（成为默认模型）</option>
                {peers.map(model => <option key={String(model.id)} value={String(model.id)}>排在 {String(model.exposed_model_id)} 之后</option>)}
              </select></label>
          </div>
        </section>
        <section aria-label="能力" className="drawer-section">
          <h4>能力</h4>
          <div className="form-grid form-grid-2">
            <label className="field"><span className="field-label">上下文</span>
              <input aria-label="上下文长度" placeholder="如 200K" value={contextText} onChange={event => setContextText(event.target.value)}/>
              <span className="field-hint">{tokenValue(contextText) ? `${formatTokenCount(tokenValue(contextText))} Tokens` : '请输入正整数 Tokens'}</span></label>
            <label className="field"><span className="field-label">最大输出</span>
              <input aria-label="最大输出" placeholder="如 32K" value={outputText} onChange={event => setOutputText(event.target.value)}/>
              <span className="field-hint">{tokenValue(outputText) ? `${formatTokenCount(tokenValue(outputText))} Tokens` : '请输入正整数 Tokens'}</span></label>
            <label className="field"><span className="field-label">扣费倍率<InfoTip text="这个模型自己的扣费倍率，与价格版本、分组的倍率相乘；不加倍填 1"/></span>
              <span className="input-suffix"><input aria-label="模型扣费倍率" inputMode="decimal" value={creditMultiplier} onChange={event => setCreditMultiplier(event.target.value)}/><span>×</span></span></label>
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
          {shared.versions.length > 0 && <div className="price-choice">
            <p className="note-info">价格表里已有 {modelId} 的价格{existing ? `：${priceSummary(existing)}` : '（还没生效）'}{sharedNames ? `，${sharedNames} 分组的 ${modelId} 按它扣费` : ''}。</p>
            <div className="segmented" role="radiogroup" aria-label="售价来源">
              <button type="button" role="radio" aria-checked={keepPrice} onClick={() => setNewPrice(false)}>沿用现有价格</button>
              <button type="button" role="radio" aria-checked={!keepPrice} onClick={() => setNewPrice(true)}>设新价格</button>
            </div>
            {!keepPrice && <p className="note-warning">已有价格的模型不能立即改价：新价格在发布后 5 分钟生效，在那之前按现有价格扣费{sharedNames ? `；它也会用于 ${sharedNames} 分组的 ${modelId}` : ''}。</p>}
          </div>}
          {!keepPrice && <>
            <details className="price-advanced listing-official">
              <summary>按官方价计算</summary>
              <div className="listing-grid">
                {OFFICIAL_LABELS.map((label, index) => <label key={label} className="field"><span className="field-label">官方{label} $</span>
                  <input aria-label={`官方${label}价`} inputMode="decimal" value={official[index]} onChange={event => setOfficial(values => values.map((value, i) => i === index ? event.target.value : value))}/></label>)}
                <label className="field"><span className="field-label">售价倍率<InfoTip text="客户每用官方 1 美元，花多少元（如 0.24）；按积分面值换成积分"/></span>
                  <input aria-label="售价倍率" inputMode="decimal" placeholder="如 0.24" value={rates.retail} onChange={event => setRates({...rates, retail: event.target.value})}/></label>
                <label className="field"><span className="field-label">成本倍率<InfoTip text="这个供应商每 1 美元官方价收多少元（如 0.08），算出采购价；按供应商分别记住"/></span>
                  <input aria-label="成本倍率" inputMode="decimal" placeholder="如 0.08" value={upstreamRate} onChange={event => setRates({...rates, upstream: {...rates.upstream, [providerId]: event.target.value}})}/></label>
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
          </>}
        </section>
        <label className="field"><span className="field-label">原因<span className="required-mark">（必填）</span></span>
          <input aria-label="上架原因" maxLength={500} value={reason} onChange={event => setReasonText(event.target.value)}/></label>
      </fieldset>
      {error && <div role="alert" className="form-error">
        <p>{error}</p>
        {conflict && <button type="button" className="btn btn-small" onClick={() => void reload()}>重新加载</button>}
      </div>}
    </div>
    <footer className="drawer-foot">
      <span className="muted">模型和它的价格一起发布，发布后立即对客户显示</span>
      <span className="drawer-foot-spacer"/>
      <button type="button" className="btn" disabled={working} onClick={close}>取消</button>
      <button type="button" className="btn btn-primary" disabled={!!blocked} title={blocked} onClick={() => void submit()}>{working ? '上架中…' : '上架'}</button>
    </footer>
  </Drawer>;
}
