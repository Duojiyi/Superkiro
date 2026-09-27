// 上架模型: one drawer. Pick an upstream model an enabled Key is authorised for (and 测试 it), say
// what customers see, choose the groups and each one's place in the list (or list it hidden
// first), price it from its official price × 计费倍率 (from the official price table, the model
// it resembles, or another model's — 按其他模型定价; credits typed directly only under 高级), then
// the preview and 上架: one publication with the entries and their first price in force at once
// (listing.ts), so a customer never sees a model that cannot be charged for.
import {useEffect, useRef, useState} from 'react';
import {adminApi, type CommercialConfig} from './api';
import {IconClose} from './components/icons';
import {Drawer} from './components/modal';
import {toast} from './components/toast';
import {InfoTip} from './components/ui';
import {formatCount, formatTokenCount} from './format';
import {buildListing, displayNameFor, groupModels, listingWarnings, MODEL_ID, MODEL_ID_RULE, type ListingInput, type Place} from './listing';
import OfficialPriceFields, {type OfficialFieldsValue} from './OfficialPriceFields';
import {chargeMicro, costsOf, creditsOf, KINDS, officialProblem, officialStart, pricingImpact, primaryCost, readSettings, yuanFor, type Four, type OfficialInput} from './officialPricing';
import {soonSecs, type PublishOutcome} from './PriceDrawer';
import {COST_FIELDS, creditsText, currentVersion, PRICE_FIELDS} from './priceChange';
import {formatMicroPrice, priceToMicroPerMillion} from './pricing';
import PricingPreview, {type PreviewPlan} from './PricingPreview';
import {marginText, timesText, typedNumber, usdText} from './pricingText';
import Probe from './Probe';
import {authorizedModels, canRoute, nameList} from './routes';
import {providerFormatLabel} from './status';
import {parseTokenInput} from './tokens';

type Row = Record<string, unknown>;
/** Where the drawer starts: the provider and upstream model to list, when known. */
export interface ListingPreset {providerId?: string; model?: string}

const FIRST = '#first';
const blank = (fields: readonly (readonly [string, string])[]) => Object.fromEntries(fields.map(([field]) => [field, '']));
const yuan = (value: number | null) => value === null ? '—' : `¥${value < 0.01 ? value.toFixed(4) : value.toFixed(2)}`;
const validReason = (text: string) => !!text.trim() && new TextEncoder().encode(text.trim()).length <= 500 && !/[\x00-\x1f\x7f-\x9f]/.test(text);
const tokenValue = (text: string) => {const value = parseTokenInput(text); return typeof value === 'number' ? value : null;};
const four = (values: Array<number | null>) => values.every(value => value !== null && Number.isFinite(value)) ? values as Four : null;
// Prices start on the server's clock: they are timed by it, not by this computer's.
const serverNow = () => adminApi.serverNowMs / 1000;
const priceSummary = (version: Row | null) => version?.pricing_mode === 'fixed'
  ? `${PRICE_FIELDS.map(([field, label]) => `${label} ${creditsText(version[field]) ?? '—'}`).join(' / ')} 积分/百万` : version ? '非固定价格' : '—';
const placeOf = (value: string): Place => value === FIRST ? {at: 'first'} : value ? {at: 'after', id: value} : {at: 'last'};
export default function ListModelDrawer({preset, config, providers, providerKeys, sampleOf, onClose, onPublish, onReload}: {
  preset: ListingPreset;
  config: CommercialConfig;
  providers: Row[];
  providerKeys: Row[];
  sampleOf: (model: string) => Four;
  onClose: () => void;
  /** `check`: what to look for if the result does not arrive. */
  onPublish: (update: {models: Row[]; versions?: Row[]}, reason: string, check: string) => Promise<PublishOutcome>;
  onReload: () => Promise<void>;
}) {
  const settings = readSettings(config.settings);
  const routable = (id: unknown) => authorizedModels(id, providerKeys).length > 0;
  const [providerId, setProviderId] = useState(() => preset.providerId
    ?? String(providers.find(provider => provider.enabled !== false && routable(provider.id))?.id ?? providers[0]?.id ?? ''));
  const [targetModel, setTargetModel] = useState(preset.model ?? '');
  const [modelIdText, setModelIdText] = useState<string | null>(null);
  const modelId = modelIdText ?? targetModel.trim();
  const [displayName, setDisplayName] = useState('');
  // The groups it goes into, each with its place: '' last, FIRST first, else the entry it follows.
  const [picked, setPicked] = useState<string[]>(() => {const first = config.groups.find(group => group.issuance_enabled !== false) ?? config.groups[0]; return first ? [String(first.id)] : [];});
  const [places, setPlaces] = useState<Record<string, string>>({});
  const [hidden, setHidden] = useState(false);
  const [reference, setReference] = useState('');
  const [contextText, setContextText] = useState('200K');
  const [outputText, setOutputText] = useState('32K');
  const [tools, setTools] = useState(true), [vision, setVision] = useState(false), [reasoning, setReasoning] = useState(false);
  const [rateMultiplier, setRateMultiplier] = useState('');
  const [creditMultiplier, setCreditMultiplier] = useState('1');
  const [mode, setMode] = useState<'official' | 'legacy'>('official');
  // Typed official fields; until then they follow the model ID, its upstream and the reference.
  const [typedFields, setTypedFields] = useState<OfficialFieldsValue | null>(null);
  const [prices, setPrices] = useState<Record<string, string>>(() => blank(PRICE_FIELDS));
  const [costs, setCosts] = useState<Record<string, string>>(() => blank(COST_FIELDS));
  // When a table already prices this model ID, the existing price is kept unless the owner asks for a new one.
  const [newPrice, setNewPrice] = useState(false);
  const [reasonText, setReasonText] = useState<string | null>(null);
  const [tokens, setTokens] = useState<string[] | null>(null);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState('');
  const [conflict, setConflict] = useState(false);
  const [preview, setPreview] = useState<PreviewPlan | null>(null);
  const alive = useRef(true);
  useEffect(() => () => {alive.current = false;}, []);
  // The steps after the confirmation read the latest configuration and callbacks, not those of the render they began in.
  const latest = useRef(config); latest.current = config;
  const publishRef = useRef(onPublish); publishRef.current = onPublish;
  const reloadRef = useRef(onReload); reloadRef.current = onReload;

  const provider = providers.find(item => item.id === providerId);
  const providerName = String(provider?.name ?? providerId);
  const groups = config.groups.filter(group => picked.includes(String(group.id)));
  const groupName = (group: Row | undefined) => String(group?.name ?? group?.id ?? '');
  const choices = authorizedModels(providerId, providerKeys);
  const listedTargets = new Set(config.models.filter(model => model.target_provider_id === providerId && picked.includes(String(model.group_id))).map(model => String(model.target_model)));
  const routeWarning = targetModel.trim() && provider && !canRoute(providerId, targetModel.trim(), providerKeys)
    ? `${providerName} 的 Key 还没有授权这个模型，先在“供应商与 Key”里勾选它并保存` : '';
  const idProblem = modelId && !MODEL_ID.test(modelId) ? MODEL_ID_RULE : '';
  // Each chosen group's price table: whether it already prices this model ID, and which other groups it charges.
  const tables = [...new Set(groups.map(group => String(group.rate_card_id ?? '')))].map(rateCardId => {
    const versions = modelId && !idProblem ? config.versions.filter(version => version.rate_card_id === rateCardId && version.model === modelId) : [];
    const others = config.groups.filter(group => group.rate_card_id === rateCardId && !picked.includes(String(group.id)) && config.models.some(model => model.group_id === group.id && model.exposed_model_id === modelId));
    return {rateCardId, versions, others, current: versions.length ? currentVersion(config.versions, rateCardId, [modelId], serverNow()) : null};
  });
  const shared = tables.filter(table => table.versions.length);
  const keepPrice = shared.length > 0 && !newPrice, needsPrice = !keepPrice || tables.some(table => !table.versions.length);
  const existing = shared[0]?.current ?? null;
  const sharedNames = nameList(shared.flatMap(table => table.others.map(group => groupName(group))));
  const reason = reasonText ?? (modelId ? `上架 ${modelId}（${providerName}）` : '');
  const mapping = {exposed_model_id: modelId, target_provider_id: providerId, target_model: targetModel.trim()};
  const referenceModel = config.models.find(item => item.id === reference) ?? null;
  const sampleTokens = four((tokens ?? sampleOf(modelId).map(String)).map(value => /^\d+$/.test(value.trim()) ? Number(value) : null));

  // The official prices it starts from: the price its table already has, the official price table
  // (the model, else its upstream), else the model it resembles.
  const auto = (): {value: OfficialFieldsValue; source: string} => {
    const own = officialStart(settings, existing, mapping);
    if (own) return {value: {usd: own.official.map(String), pricedAs: null, priceMultiplier: String(own.input?.priceMultiplier ?? settings.defaultPrice ?? ''), costMultiplier: ''},
      source: own.from === 'version' ? '来自价格表里现有价格的官方价' : own.from === 'model' ? `来自官方价表 ${own.name}` : `来自官方价表 ${own.name}（上游模型）`};
    const referenced = referenceModel ? officialStart(settings, currentVersion(config.versions, config.groups.find(group => group.id === referenceModel.group_id)?.rate_card_id, [referenceModel.exposed_model_id, referenceModel.target_model], serverNow()), referenceModel) : null;
    if (referenced) return {value: {usd: referenced.official.map(String), pricedAs: referenced.from === 'version' ? null : referenced.name, priceMultiplier: String(referenced.input?.priceMultiplier ?? settings.defaultPrice ?? ''), costMultiplier: ''},
      source: `按参照模型 ${String(referenceModel?.exposed_model_id)} 的官方价`};
    return {value: {usd: ['', '', '', ''], pricedAs: null, priceMultiplier: settings.defaultPrice !== null ? String(settings.defaultPrice) : '', costMultiplier: ''}, source: '没有官方价：填四项官方价，或按其他模型定价'};
  };
  const started = auto(), fields = typedFields ?? started.value;
  const pricedNote = fields.pricedAs ?? (!typedFields && referenceModel && started.source.startsWith('按参照模型') ? String(referenceModel.exposed_model_id) : null);
  const cost = primaryCost(settings, mapping, four(fields.usd.map(typedNumber)) ?? [0, 0, 0, 0]);
  const officialInput = (): OfficialInput => {
    const official = four(fields.usd.map(typedNumber));
    if (!official) throw new Error('填写四项官方价（美元 / 百万 Tokens，免费填 0），或按其他模型定价');
    const priceMultiplier = typedNumber(fields.priceMultiplier), costMultiplier = cost.costMultiplier ?? typedNumber(fields.costMultiplier);
    if (priceMultiplier === null || !Number.isFinite(priceMultiplier)) throw new Error('填写计费倍率');
    if (costMultiplier === null || !Number.isFinite(costMultiplier)) throw new Error('这个供应商还没有成本倍率：填一个，或先在“定价设置”里设');
    if (settings.face === null) throw new Error('积分面值没有读到，请重新加载');
    const input = {official, priceMultiplier, costMultiplier, basis: primaryCost(settings, mapping, official).basis, usdCny: settings.usdCny, face: settings.face};
    const problem = officialProblem(input);
    if (problem) throw new Error(problem);
    return input;
  };
  let results: Parameters<typeof OfficialPriceFields>[0]['results'] = null, newCredits: Four | null = null, newCost: Four | null = null, priceProblem = '';
  try {
    if (mode === 'official') {
      const input = officialInput();
      newCredits = creditsOf(input); newCost = costsOf(input);
      results = {yuan: input.official.map(usd => yuanFor(usd, input.priceMultiplier, input.usdCny)) as Four, before: null, after: newCredits, cost: newCost};
    } else {
      newCredits = PRICE_FIELDS.map(([field]) => priceToMicroPerMillion((prices[field] ?? '').trim(), 'million')) as Four;
      newCost = four(COST_FIELDS.map(([field]) => typedNumber(costs[field] ?? '')));
    }
  } catch (cause) {priceProblem = cause instanceof Error ? cause.message : String(cause);}
  const group = groups[0];
  const factor = Number(group?.margin_multiplier ?? 1) * (Number(creditMultiplier) || 1);
  let sampleText = '';
  if (newCredits && sampleTokens && settings.face !== null) {
    const charged = chargeMicro(newCredits, sampleTokens, factor), revenue = charged / 1_000_000 * settings.face;
    const spent = newCost ? sampleTokens.reduce((sum, count, kind) => sum + count * newCost![kind] / 1_000_000, 0) : null;
    sampleText = `${formatMicroPrice(charged)} 积分（≈ ${yuan(revenue)}）· 成本 ≈ ${yuan(spent)} · 毛利${spent === null || revenue <= 0 ? '暂不计算' : `约 ${marginText((revenue - spent) / revenue)}`}`;
  }
  const sampleLabel = sampleTokens ? `示例 ${formatCount(sampleTokens[0])} 输入 + ${formatCount(sampleTokens[1])} 输出${sampleTokens[2] ? ` + ${formatCount(sampleTokens[2])} 缓存写` : ''}${sampleTokens[3] ? ` + ${formatCount(sampleTokens[3])} 缓存读` : ''}` : '示例用量须为非负整数';
  const warnings = listingWarnings({targetModel: targetModel.trim(), modelId, reasoning, contextWindow: tokenValue(contextText), maxOutput: tokenValue(outputText)});
  const blocked = working ? '正在上架' : !picked.length ? '至少选一个分组' : idProblem || (!validReason(reason) ? '填写原因后可预览（最多约 160 字）' : undefined);

  const applyReference = (id: string) => {
    setReference(id);
    const model = config.models.find(item => item.id === id);
    if (!model) return;
    if (!picked.includes(String(model.group_id))) setPicked(value => [...value, String(model.group_id)]);
    setPlaces(value => ({...value, [String(model.group_id)]: id}));
    setContextText(String(model.context_window ?? '')); setOutputText(String(model.max_output ?? ''));
    setTools(model.supports_tools === true); setVision(model.supports_vision === true); setReasoning(model.supports_reasoning === true);
    setRateMultiplier(typeof model.rate_multiplier === 'number' ? String(model.rate_multiplier) : '');
    setCreditMultiplier(String(model.credit_multiplier ?? 1));
    setTypedFields(null);
    const card = config.groups.find(item => item.id === model.group_id)?.rate_card_id;
    const price = currentVersion(config.versions, card, [model.exposed_model_id, model.target_model], serverNow());
    if (price?.pricing_mode === 'fixed') setPrices(Object.fromEntries(PRICE_FIELDS.map(([field]) => [field, creditsText(price[field]) ?? ''])));
  };
  const toggleGroup = (id: string, on: boolean) => setPicked(value => on ? [...value.filter(item => item !== id), id].sort((a, b) => config.groups.findIndex(group => group.id === a) - config.groups.findIndex(group => group.id === b)) : value.filter(item => item !== id));

  const input = (): ListingInput => {
    const [first, ...rest] = picked;
    return {providerId, targetModel, modelId, displayName, groupId: first ?? '', place: placeOf(places[first] ?? ''), also: rest.map(id => ({groupId: id, place: placeOf(places[id] ?? '')})),
      hidden, contextWindow: tokenValue(contextText), maxOutput: tokenValue(outputText), tools, vision, reasoning, rateMultiplier, creditMultiplier, keepPrice,
      official: needsPrice && mode === 'official' ? officialInput() : null, prices, costs, currency: 'CNY'};
  };
  const build = (base: CommercialConfig) => buildListing(input(), {config: base, providers, keys: providerKeys, nowSecs: serverNow(), effectiveSecs: soonSecs()});

  const open = () => {
    if (blocked) return;
    setError(''); setConflict(false);
    let listing: ReturnType<typeof buildListing>;
    try {listing = build(config);} catch (cause) {setError(cause instanceof Error ? cause.message : String(cause)); return;}
    const {mappings, versions} = listing, now = serverNow();
    const shownName = String(listing.mapping.display_name ?? displayNameFor(modelId));
    const placeText = (entry: Row) => {
      const value = places[String(entry.group_id)] ?? '', peers = groupModels(config.models, entry.group_id);
      return value === FIRST ? '排在最前（成为默认模型）' : value ? `排在 ${String(peers.find(model => model.id === value)?.exposed_model_id ?? value)} 之后` : '排在最后';
    };
    const capabilities = [tools && '工具', vision && '图片', reasoning && '推理'].filter(Boolean).join('、') || '无';
    const official = versions.find(version => version.official)?.official as Row | undefined;
    const models = [...config.models.filter(model => !listing.models.some(next => next.id === model.id)), ...listing.models];
    const after = [...config.versions, ...versions.map(version => version.effective_from_secs === 0 ? {...version, effective_from_secs: now} : version)];
    const rows = pricingImpact({settings, versions: config.versions}, {settings, versions: after}, {models, groups: config.groups, nowSecs: now, effectiveSecs: versions.some(version => version.effective_from_secs) ? soonSecs() : now,
      sample: other => other === modelId && sampleTokens ? sampleTokens : sampleOf(other)}).filter(row => row.model === modelId);
    const officialUsd = official ? {official: [official.input_usd_per_m, official.output_usd_per_m, official.cache_creation_usd_per_m, official.cache_read_usd_per_m] as Four} : null;
    setPreview({title: `上架 ${modelId}？`, rows, confirmLabel: '上架', empty: `沿用价格表里现有的价格（${priceSummary(existing)}）`,
      zeros: officialUsd ? officialUsd.official.map((usd, index) => usd === 0 ? `${modelId} 的${KINDS[index]}` : '').filter(Boolean) : [],
      facts: [
        `客户看到：${shownName}（${modelId}）${hidden ? ' · 先隐藏，客户暂时看不到' : ''}`,
        ...mappings.map(entry => `${groupName(config.groups.find(item => item.id === entry.group_id))}：${placeText(entry)}`),
        `线路：${providerName} / ${String(listing.mapping.target_model)}`,
        versions.length ? (officialUsd ? `售价：官方价 ${officialUsd.official.map(usd => usdText(usd)).join(' / ')}${pricedNote ? `（按 ${pricedNote} 定价）` : ''} × 计费倍率 ${timesText(Number(official!.price_multiplier))}` : `售价：直接填积分（旧版）· ${priceSummary(versions[0])}`)
          + ` · ${versions.every(version => version.effective_from_secs === 0) ? '上架即生效' : '已有价格的价格表约 1 分钟后换新价格，在那之前按现有价格'}`
          : `售价：沿用价格表里现有的价格（${priceSummary(existing)}）`,
        ...(versions.length && listing.sharedWith.length ? [`新价格同时用于 ${nameList(listing.sharedWith.map(item => groupName(item)))} 分组的 ${modelId}（同一价格表）`] : []),
        `上下文 ${formatTokenCount(listing.mapping.context_window)} · 最大输出 ${formatTokenCount(listing.mapping.max_output)} · 能力：${capabilities} · 模型倍率 ${String(listing.mapping.credit_multiplier)} · 显示倍率 ${typeof listing.mapping.rate_multiplier === 'number' ? `${listing.mapping.rate_multiplier}x` : '按价格自动换算'}`,
        ...warnings.map(warning => <span className="is-warning">{warning}</span>),
        `原因：${reason.trim()}${pricedNote && !reason.includes(`按 ${pricedNote} 定价`) ? `（按 ${pricedNote} 定价）` : ''}`,
      ],
      consequence: hidden ? '发布后先不对客户显示；核对好之后在列表里点“重新上架”。' : '发布后立即对客户显示：客户刷新模型列表或重启 Kiro 后可见，新请求马上可以用。'});
  };
  const confirm = async () => {
    setPreview(null);
    // Built again on the configuration as it is now: its places and IDs are the latest ones.
    let listing: ReturnType<typeof buildListing>;
    try {listing = build(latest.current);} catch (cause) {setError(cause instanceof Error ? cause.message : String(cause)); return;}
    setWorking(true);
    const names = nameList(listing.mappings.map(entry => groupName(latest.current.groups.find(item => item.id === entry.group_id))));
    const outcome = await publishRef.current({models: listing.models, ...(listing.versions.length ? {versions: listing.versions} : {})},
      `${reason.trim()}${pricedNote && !reason.includes(`按 ${pricedNote} 定价`) ? `（按 ${pricedNote} 定价）` : ''}`,
      `“模型与定价”里有没有 ${modelId}（${names}）、“价格版本”里有没有它的价格`);
    if (!alive.current) return;
    setWorking(false);
    if (outcome.ok) {toast.success(`已上架 ${modelId}${hidden ? '（先隐藏）' : ''}`); onClose(); return;}
    if (outcome.uncertain) {onClose(); return;}
    setConflict(!!outcome.conflict); setError(outcome.message);
  };

  const close = () => {if (!working) onClose();};
  const reload = async () => {
    setWorking(true);
    try {await reloadRef.current();} finally {if (alive.current) {setWorking(false); setError(''); setConflict(false);}}
  };
  const costText = cost.costMultiplier === null ? '' : `${cost.basis ? `上游计费基准 ${cost.basis.map(usd => usdText(usd)).join(' / ')}` : '官方价'} × ${timesText(cost.costMultiplier)}（${cost.source === 'route' ? '这条线路' : cost.source === 'provider' ? '供应商' : '默认'}）`;

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
                {providers.map(item => <option key={String(item.id)} value={String(item.id)}>{String(item.name ?? item.id)}{providerFormatLabel(item) === 'OpenAI' ? ' · OpenAI' : ''}{item.enabled === false ? '（已停用）' : ''}</option>)}
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
          <label className="field"><span className="field-label">参照现有模型<InfoTip text="复制它的上下文、能力、模型倍率、显示倍率，按它的官方价定价（这个模型自己没有官方价时），并排在它后面"/></span>
            <select aria-label="参照现有模型" value={reference} onChange={event => applyReference(event.target.value)}>
              <option value="">不参照</option>
              {config.models.map(model => <option key={String(model.id)} value={String(model.id)}>{String(model.exposed_model_id)}（{groupName(config.groups.find(item => item.id === model.group_id))} · {String(model.target_provider_id)}）</option>)}
            </select></label>
          <div className="form-grid form-grid-2">
            <label className="field"><span className="field-label">模型 ID<InfoTip text="客户在 Kiro 里看到并请求的 ID，默认同上游模型"/></span>
              <input aria-label="模型 ID" className="mono" aria-invalid={!!idProblem} value={modelId} onChange={event => setModelIdText(event.target.value)}/>
              {idProblem && <span className="field-warning">{idProblem}</span>}</label>
            <label className="field"><span className="field-label">显示名称</span>
              <input aria-label="显示名称" placeholder={modelId ? `留空显示为 ${displayNameFor(modelId)}` : '留空自动生成'} value={displayName} onChange={event => setDisplayName(event.target.value)}/></label>
          </div>
          <fieldset className="field listing-groups" aria-label="分组">
            <legend className="field-label">分组与位置<InfoTip text="可以一次上架到几个分组，各自排位置；排在最前的是这个分组在 Kiro 里的默认模型"/></legend>
            {config.groups.map(item => {
              const id = String(item.id), on = picked.includes(id), peers = groupModels(config.models, id);
              return <div key={id} className="listing-group">
                <label className="check-field"><input type="checkbox" checked={on} onChange={event => toggleGroup(id, event.target.checked)}/>{groupName(item)}</label>
                {on && <select aria-label={`${groupName(item)} 的位置`} value={places[id] ?? ''} onChange={event => setPlaces(value => ({...value, [id]: event.target.value}))}>
                  <option value="">排在最后</option>
                  <option value={FIRST}>排在最前（成为默认模型）</option>
                  {peers.map(model => <option key={String(model.id)} value={String(model.id)}>排在 {String(model.exposed_model_id)} 之后</option>)}
                </select>}
              </div>;
            })}
          </fieldset>
          <label className="check-field"><input type="checkbox" checked={hidden} onChange={event => setHidden(event.target.checked)}/>上架后先隐藏<InfoTip text="客户暂时看不到；核对好之后在列表里点“重新上架”"/></label>
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
            <label className="field"><span className="field-label">模型倍率<InfoTip text="这个模型自己的倍率，与分组倍率、版本倍率相乘；不加倍填 1"/></span>
              <span className="input-suffix"><input aria-label="模型倍率" inputMode="decimal" value={creditMultiplier} onChange={event => setCreditMultiplier(event.target.value)}/><span>×</span></span></label>
            <label className="field"><span className="field-label">显示倍率<InfoTip text="客户端模型列表里显示的倍率，只是给客户看的，不影响扣费；留空按价格自动换算"/></span>
              <span className="input-suffix"><input aria-label="显示倍率" inputMode="decimal" placeholder="如 2.2" value={rateMultiplier} onChange={event => setRateMultiplier(event.target.value)}/><span>×</span></span>
              <span className="field-hint">只影响客户端显示，不影响扣费</span></label>
          </div>
          <div className="check-row">
            <label className="check-field"><input type="checkbox" checked={tools} onChange={event => setTools(event.target.checked)}/>工具</label>
            <label className="check-field"><input type="checkbox" checked={vision} onChange={event => setVision(event.target.checked)}/>图片</label>
            <label className="check-field"><input type="checkbox" checked={reasoning} onChange={event => setReasoning(event.target.checked)}/>推理<InfoTip text="只在上游支持思考时打开，否则客户选推理强度时会报错"/></label>
          </div>
          {warnings.map(warning => <p key={warning} className="field-warning">{warning}</p>)}
        </section>
        <section aria-label="售价" className="drawer-section">
          <h4>售价</h4>
          {shared.length > 0 && <div className="price-choice">
            <p className="note-info">价格表里已有 {modelId} 的价格{existing ? `：${priceSummary(existing)}` : '（还没生效）'}{sharedNames ? `，${sharedNames} 分组的 ${modelId} 按它扣费` : ''}。</p>
            <div className="segmented" role="radiogroup" aria-label="售价来源">
              <button type="button" role="radio" aria-checked={keepPrice} onClick={() => setNewPrice(false)}>沿用现有价格</button>
              <button type="button" role="radio" aria-checked={!keepPrice} onClick={() => setNewPrice(true)}>设新价格</button>
            </div>
            {!keepPrice && <p className="note-warning">已有价格的模型不能立即改价：新价格约 1 分钟后生效，在那之前按现有价格扣费{sharedNames ? `；它也会用于 ${sharedNames} 分组的 ${modelId}` : ''}。</p>}
          </div>}
          {needsPrice && <>
            {mode === 'official' ? <OfficialPriceFields value={fields} onChange={setTypedFields} settings={settings} source={started.source} costSource={costText} needsCost={cost.costMultiplier === null} results={results}/>
              : <>
                <p className="note-warning">旧版定价：积分直接填，不记官方价；改积分面值时它的积分不会跟着重算。</p>
                <div className="listing-grid">
                  {PRICE_FIELDS.map(([field, label]) => <label key={field} className="field"><span className="field-label">{label} 积分/百万</span>
                    <input aria-label={`${label}售价`} inputMode="decimal" value={prices[field] ?? ''} onChange={event => setPrices({...prices, [field]: event.target.value})}/></label>)}
                  {COST_FIELDS.map(([field, label]) => <label key={field} className="field"><span className="field-label">采购{label} ¥/百万</span>
                    <input aria-label={`采购${label}价`} type="number" min="0" step="any" value={costs[field] ?? ''} onChange={event => setCosts({...costs, [field]: event.target.value})}/></label>)}
                </div>
              </>}
            {priceProblem && <p className="field-warning">{priceProblem}</p>}
            <label className="check-field legacy-switch"><input type="checkbox" checked={mode === 'legacy'} onChange={event => setMode(event.target.checked ? 'legacy' : 'official')}/>高级：直接填积分（旧版，不记官方价）</label>
            <section className="pricing-preview" aria-label="扣费示例">
              <div role="status" className={`pricing-result${!sampleText ? ' pricing-result-error' : sampleText.includes('毛利约 −') ? ' is-losing' : ''}`}>
                <p>{sampleLabel} → {sampleText ? <strong>{sampleText}</strong> : <span>填好价格后显示示例扣费</span>}</p>
              </div>
              <details className="sample-tokens"><summary>示例用量</summary>
                <div className="listing-grid pricing-tokens">{KINDS.map((label, index) => <label key={label} className="field"><span className="field-label">{label} Tokens</span>
                  <input inputMode="numeric" value={(tokens ?? sampleOf(modelId).map(String))[index]} onChange={event => setTokens(values => (values ?? sampleOf(modelId).map(String)).map((value, i) => i === index ? event.target.value : value))}/></label>)}</div>
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
      <span className="muted">{hidden ? '模型和它的价格一起发布，先隐藏' : '模型和它的价格一起发布，发布后立即对客户显示'}</span>
      <span className="drawer-foot-spacer"/>
      <button type="button" className="btn" disabled={working} onClick={close}>取消</button>
      <button type="button" className="btn btn-primary" disabled={!!blocked} title={blocked} onClick={open}>{working ? '上架中…' : '预览上架'}</button>
    </footer>
    {preview && <PricingPreview plan={preview} providers={providers} groups={config.groups} onCancel={() => setPreview(null)} onConfirm={() => void confirm()}/>}
  </Drawer>;
}
