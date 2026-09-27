// 调价: a model's new price in each price table that charges it (every group sharing a table pays
// it), from its official price — the price in force's own block, else the official price table
// (for the model, else its upstream model), else another model's (按其他模型定价) — × 计费倍率, its
// cost recorded as billing will cost its primary route. Credits typed directly stay under 高级 as a
// legacy price without an official block. A model's first price starts at once; a later one about a
// minute on (尽快) or at a chosen time. Scheduled prices can be withdrawn here, and the previous
// price's inputs brought back as a new version. Every change ends in the preview, then one
// publication against the configuration read.
import {useEffect, useRef, useState} from 'react';
import {type CommercialConfig} from './api';
import {pricingNow} from './clock';
import {ask} from './components/confirm';
import {IconClose} from './components/icons';
import {Drawer} from './components/modal';
import {InfoTip, Tag} from './components/ui';
import {formatCount, formatDateTime, formatFullDateTime} from './format';
import OfficialPriceFields, {type OfficialFieldsValue} from './OfficialPriceFields';
import {chargeMicro, costOfRoute, costsOf, creditsOf, creditsOfVersion, freeTime, KINDS, multipliers, officialOf, officialProblem, officialStart, officialVersion, pricingImpact, primaryCost, readSettings, yuanFor, type Four, type OfficialInput} from './officialPricing';
import {buildPriceVersion, COST_FIELDS, creditsText, currentVersion, percentChange, PRICE_FIELDS, scheduledVersions, versionIdFor} from './priceChange';
import {formatMicroPrice, priceToMicroPerMillion} from './pricing';
import PricingPreview, {type PreviewPlan} from './PricingPreview';
import {marginText, timesText, typedNumber, usdText} from './pricingText';
import type {PublishOutcome} from './refusal';
import {nameList} from './routes';

type Row = Record<string, unknown>;
export type {PublishOutcome};
export type PricePublish = (update: {versions?: Row[]; cancelled_versions?: string[]}, reason: string, done: string, check: string) => Promise<PublishOutcome>;

const serverNow = pricingNow;
/** 尽快: the next whole minute at least a minute away, so it is still ahead when the publication arrives. */
export const soonSecs = () => Math.ceil((serverNow() + 60) / 60) * 60;
const toLocalInput = (secs: number) => {
  const date = new Date(secs * 1000);
  return new Date(date.getTime() - date.getTimezoneOffset() * 60000).toISOString().slice(0, 16);
};
const yuan = (value: number | null) => value === null ? '—' : `¥${value < 0.01 ? value.toFixed(4) : value.toFixed(2)}`;
const validReason = (text: string) => !!text.trim() && new TextEncoder().encode(text.trim()).length <= 500 && !/[\x00-\x1f\x7f-\x9f]/.test(text);
const four = (values: Array<number | null>) => values.every(value => value !== null && Number.isFinite(value)) ? values as Four : null;

/** The inputs of a price version, as the drawer's fields hold them. */
export function fieldsFrom(version: Row | null, fallback: {usd?: Four | null; priceMultiplier?: number | null}): OfficialFieldsValue {
  const input = officialOf(version);
  return {usd: (input?.official ?? fallback.usd ?? ['', '', '', '']).map(value => String(value)), pricedAs: null,
    priceMultiplier: String(input?.priceMultiplier ?? fallback.priceMultiplier ?? ''), costMultiplier: input ? String(input.costMultiplier) : ''};
}

export default function PriceDrawer({mappings, config, providers, sampleOf, onClose, onPublish, onReload}: {
  /** The model's entries, one per group; the first is the one it was opened from. */
  mappings: Row[];
  config: CommercialConfig;
  providers: Row[];
  sampleOf: (model: string) => {tokens: Four; count: number};
  onClose: () => void;
  onPublish: PricePublish;
  onReload: () => Promise<void>;
}) {
  const model = mappings[0], name = String(model.exposed_model_id ?? model.id);
  const names = [model.exposed_model_id, model.target_model];
  const settings = readSettings(config.settings), nowSecs = serverNow();
  // Each price table that charges this model, with its groups and its prices now and to come.
  const tables = [...new Set(mappings.map(mapping => config.groups.find(group => group.id === mapping.group_id)?.rate_card_id).filter((id): id is string => typeof id === 'string'))]
    .map(rateCardId => ({rateCardId, name: String(config.rate_cards.find(card => card.id === rateCardId)?.name ?? rateCardId),
      groups: config.groups.filter(group => group.rate_card_id === rateCardId && mappings.some(mapping => mapping.group_id === group.id)),
      current: currentVersion(config.versions, rateCardId, names, nowSecs), scheduled: scheduledVersions(config.versions, rateCardId, names, nowSecs),
      priced: config.versions.some(version => version.rate_card_id === rateCardId && version.model === name)}));
  const [ticked, setTicked] = useState<string[]>(() => tables.map(table => table.rateCardId));
  const chosen = tables.filter(table => ticked.includes(table.rateCardId)), first = chosen[0] ?? tables[0];
  const current = first?.current ?? null, currentInput = officialOf(current);
  const start = officialStart(settings, current, model);
  const [mode, setMode] = useState<'official' | 'legacy'>('official');
  const [fields, setFields] = useState<OfficialFieldsValue>(() => fieldsFrom(current, {usd: start?.official, priceMultiplier: settings.defaultPrice}));
  const legacyRate = current?.currency === 'USD' ? settings.legacyRate ?? NaN : 1;
  const [prices, setPrices] = useState<Record<string, string>>(() => Object.fromEntries(PRICE_FIELDS.map(([field]) => [field, creditsText(current?.[field]) ?? ''])));
  const [costs, setCosts] = useState<Record<string, string>>(() => Object.fromEntries(COST_FIELDS.map(([field]) => [field, typeof current?.[field] === 'number' ? String(+(Number(current[field]) * legacyRate).toFixed(9)) : ''])));
  const [versionMultiplier, setVersionMultiplier] = useState(() => String(current?.margin_multiplier ?? 1));
  const [timing, setTiming] = useState<'soon' | 'custom'>('soon');
  const [customTime, setCustomTime] = useState(() => toLocalInput(soonSecs() + 3600));
  const initialSample = sampleOf(name);
  const [tokens, setTokens] = useState(() => initialSample.tokens.map(String));
  const [reason, setReason] = useState('');
  const [error, setError] = useState('');
  const [conflict, setConflict] = useState(false);
  const [working, setWorking] = useState(false);
  const [preview, setPreview] = useState<PreviewPlan | null>(null);
  const alive = useRef(true);
  useEffect(() => () => {alive.current = false;}, []);

  const cost = primaryCost(settings, model, four(fields.usd.map(typedNumber)) ?? [0, 0, 0, 0]);
  const sample = four(tokens.map(value => /^\d+$/.test(value.trim()) ? Number(value) : null));
  // The official inputs as typed, or why they cannot be used yet.
  const officialInput = (): OfficialInput => {
    const official = four(fields.usd.map(typedNumber));
    if (!official) throw new Error('填写四项官方价（美元 / 百万 Tokens，免费填 0），或按其他模型定价');
    const priceMultiplier = typedNumber(fields.priceMultiplier), costMultiplier = cost.costMultiplier ?? typedNumber(fields.costMultiplier);
    if (priceMultiplier === null || !Number.isFinite(priceMultiplier)) throw new Error('填写计费倍率');
    if (costMultiplier === null || !Number.isFinite(costMultiplier)) throw new Error('主线路还没有成本倍率：填一个，或先在“定价设置”里给供应商设');
    if (settings.face === null) throw new Error('积分面值没有读到，请重新加载');
    const input = {official, priceMultiplier, costMultiplier, basis: primaryCost(settings, model, official).basis, usdCny: settings.usdCny, face: settings.face};
    const problem = officialProblem(input);
    if (problem) throw new Error(problem);
    return input;
  };
  // The new version for each chosen table at `when` (0 for a table's first price), with fresh IDs.
  const build = (when: number): Row[] => {
    const taken = config.versions.map(version => version.id);
    return chosen.map(table => {
      const at = table.priced ? when : 0, id = versionIdFor(name, at || nowSecs, taken);
      taken.push(id);
      if (mode === 'official') return officialVersion(officialInput(), {id, rateCardId: table.rateCardId, model: name, effectiveSecs: at});
      const version = buildPriceVersion({prices, costs, currency: 'CNY', multiplier: versionMultiplier, effectiveSecs: at, id}, {model: name, rateCardId: table.rateCardId, versions: config.versions, nowSecs: serverNow(), base: table.current});
      delete version.official;
      return version;
    });
  };
  let results: Parameters<typeof OfficialPriceFields>[0]['results'] = null, newCredits: Four | null = null, newProblem = '';
  try {
    if (mode === 'official') {
      const input = officialInput();
      newCredits = creditsOf(input);
      results = {yuan: input.official.map(usd => yuanFor(usd, input.priceMultiplier, input.usdCny)) as Four, before: creditsOfVersion(current), after: newCredits, cost: costsOf(input)};
    } else newCredits = PRICE_FIELDS.map(([field]) => priceToMicroPerMillion((prices[field] ?? '').trim(), 'million')) as Four;
  } catch (cause) {newProblem = cause instanceof Error ? cause.message : String(cause);}
  // A sample request at the new price and at the current one: credits, ¥ and margin on the primary route.
  const factor = (version: Row | null, multiplier?: number) => multiplier ?? multipliers(version, config.groups.find(group => group.id === model.group_id), model).product;
  const primaryCostNow = costOfRoute(settings, config.versions, first?.rateCardId, {provider_id: String(model.target_provider_id), target_model: String(model.target_model)}, model, nowSecs);
  const sampleLine = (credits: Four | null, version: Row | null, costPerM: Four | null, multiplier?: number) => {
    if (!credits || !sample || settings.face === null) return null;
    const charged = chargeMicro(credits, sample, factor(version, multiplier)), revenue = charged / 1_000_000 * settings.face;
    const spent = costPerM ? sample.reduce((sum, count, kind) => sum + count * costPerM[kind] / 1_000_000, 0) : null;
    return `${formatMicroPrice(charged)} 积分（≈ ${yuan(revenue)}）· 成本 ≈ ${yuan(spent)} · 毛利${spent === null || revenue <= 0 ? '暂不计算' : `约 ${marginText((revenue - spent) / revenue)}`}`;
  };
  const legacyFactor = () => {const value = Number(versionMultiplier); return Number.isFinite(value) ? value * multipliers(null, config.groups.find(group => group.id === model.group_id), model).product : NaN;};
  const newCost = mode === 'official' ? results?.cost ?? null : four(COST_FIELDS.map(([field]) => typedNumber(costs[field] ?? '')));
  const sampleNew = sampleLine(newCredits, null, newCost, mode === 'official' ? multipliers(null, config.groups.find(group => group.id === model.group_id), model).product : legacyFactor());
  const sampleNow = sampleLine(creditsOfVersion(current), current, primaryCostNow.perM);
  const sampleLabel = sample ? `示例 ${formatCount(sample[0])} 输入 + ${formatCount(sample[1])} 输出${sample[2] ? ` + ${formatCount(sample[2])} 缓存写` : ''}${sample[3] ? ` + ${formatCount(sample[3])} 缓存读` : ''}` : '示例用量须为非负整数';
  const blocked = working ? '正在发布' : !tables.length ? '这个模型的分组没有价格表，不能调价' : !chosen.length ? '至少选一个价格表' : !validReason(reason) ? '填写原因后可预览（最多约 160 字）' : undefined;
  const previous = current ? config.versions.filter(version => version.rate_card_id === first.rateCardId && version.model === current.model && Number(version.effective_from_secs) < Number(current.effective_from_secs))
    .sort((a, b) => Number(b.effective_from_secs) - Number(a.effective_from_secs))[0] ?? null : null;

  const restore = () => {
    if (!previous) return;
    const input = officialOf(previous);
    if (input) {setMode('official'); setFields(fieldsFrom(previous, {}));}
    else {
      setMode('legacy');
      const rate = previous.currency === 'USD' ? settings.legacyRate ?? NaN : 1;
      setPrices(Object.fromEntries(PRICE_FIELDS.map(([field]) => [field, creditsText(previous[field]) ?? ''])));
      setCosts(Object.fromEntries(COST_FIELDS.map(([field]) => [field, typeof previous[field] === 'number' ? String(+(Number(previous[field]) * rate).toFixed(9)) : ''])));
      setVersionMultiplier(String(previous.margin_multiplier ?? 1));
    }
    setReason(value => value || `恢复上一版价格（${formatDateTime(Number(previous.effective_from_secs))} 起的那一版）`);
  };
  const cancel = async (version: Row) => {
    const answer = await ask({title: '取消这个排期价格？', facts: [`${name}：${formatFullDateTime(Number(version.effective_from_secs))} 起`, `输入 ${creditsText(version.fixed_input_credit_per_m) ?? '—'} / 输出 ${creditsText(version.fixed_output_credit_per_m) ?? '—'} 积分/百万`, `版本 ${String(version.id)}`],
      consequence: '撤回后它不会生效，现在的价格照常。', confirmLabel: '取消排期', reason: {label: '原因', required: true, maxLength: 160, placeholder: '例：促销取消'}});
    if (!answer.confirmed || !alive.current) return;
    setWorking(true);
    const outcome = await onPublish({cancelled_versions: [String(version.id)]}, answer.reason, `已取消 ${name} ${formatFullDateTime(Number(version.effective_from_secs))} 的排期价格`, `“价格版本”里 ${String(version.id)} 是否已撤回`);
    if (!alive.current) return;
    setWorking(false);
    if (outcome.ok) return;
    if (outcome.uncertain) {onClose(); return;}
    setError(outcome.message); setConflict(!!outcome.conflict);
  };

  // When it starts: at once as a table's first price; else about a minute on (the first minute
  // without a price of this model) or at the time chosen.
  const priced = (): Array<[unknown, unknown]> => chosen.filter(table => table.priced).map(table => [table.rateCardId, name]);
  const effectiveTime = () => chosen.every(table => !table.priced) ? 0 : timing === 'soon' ? freeTime(config.versions, priced(), soonSecs()) : Math.floor(new Date(customTime).getTime() / 1000);
  const open = () => {
    if (blocked) return;
    setError(''); setConflict(false);
    const when = effectiveTime();
    if (when !== 0 && (!Number.isSafeInteger(when) || when <= serverNow())) {setError('生效时间需晚于现在'); return;}
    if (when !== 0 && freeTime(config.versions, priced(), when) !== when) {setError('这一时刻已有这个模型的价格版本，请换一个生效时间'); return;}
    let versions: Row[];
    try {versions = build(when);} catch (cause) {setError(cause instanceof Error ? cause.message : String(cause)); return;}
    const unchanged = versions.every((version, index) => {const before = chosen[index].current; return !!before && JSON.stringify(creditsOfVersion(before)) === JSON.stringify(creditsOfVersion(version))
      && JSON.stringify(officialOf(before)) === JSON.stringify(officialOf(version)) && Number(before.margin_multiplier ?? 1) === Number(version.margin_multiplier);});
    if (unchanged) {setError('价格没有变化'); return;}
    const now = serverNow(), after = [...config.versions, ...versions.map(version => version.effective_from_secs === 0 ? {...version, effective_from_secs: now} : version)];
    const rows = pricingImpact({settings, versions: config.versions}, {settings, versions: after}, {models: config.models, groups: config.groups, nowSecs: now, effectiveSecs: when || now,
      sample: other => other === name && sample ? sample : sampleOf(other).tokens});
    const input = mode === 'official' ? officialInput() : null;
    const facts = [
      input ? `官方价 ${input.official.map(usd => usdText(usd)).join(' / ')}${fields.pricedAs ? `（按 ${fields.pricedAs} 定价）` : start ? `（来自${start.from === 'version' ? '现行价格' : `官方价表 ${start.name}`}）` : ''} × 计费倍率 ${timesText(input.priceMultiplier)}${currentInput && currentInput.priceMultiplier !== input.priceMultiplier ? `（原 ${timesText(currentInput.priceMultiplier)}）` : ''}`
        : `直接填积分（旧版，不记官方价）· 版本倍率 ${versionMultiplier}`,
      ...(input ? [`成本按主线路：${cost.basis ? `计费基准 ${cost.basis.map(usd => usdText(usd)).join(' / ')}` : '官方价'} × ${timesText(input.costMultiplier)}`] : []),
      `同一价格表的分组：${nameList(chosen.flatMap(table => table.groups.map(group => String(group.name ?? group.id))))}（都按新价格扣费）`,
      `生效：${when === 0 ? '发布即生效（这个价格表里它的第一个价格）' : `${timing === 'soon' ? '尽快，' : ''}${formatFullDateTime(when)} 起`}`,
      `版本 ${versions.map(version => String(version.id)).join('、')}`, `原因：${reason.trim()}`];
    const zeros = input ? input.official.map((usd, index) => usd === 0 ? `${name} 的${KINDS[index]}` : '').filter(Boolean) : [];
    setPreview({title: `发布 ${name} 的新价格？`, facts, rows, zeros, confirmLabel: '发布调价',
      consequence: '到生效时间后，新请求按新价格扣费；已开始的请求按原价格结算。'});
  };
  const confirm = async () => {
    setPreview(null);
    const when = effectiveTime();
    if (when !== 0 && when <= serverNow()) {setError('生效时间已过，请重新选择'); return;}
    let versions: Row[];
    try {versions = build(when);} catch (cause) {setError(cause instanceof Error ? cause.message : String(cause)); return;}
    const note = fields.pricedAs && mode === 'official' ? `（按 ${fields.pricedAs} 定价）` : '';
    setWorking(true);
    const outcome = await onPublish({versions}, `${reason.trim()}${note && !reason.includes(`按 ${fields.pricedAs} 定价`) ? note : ''}`, `已发布 ${name} 的新价格`, `“价格版本”里有没有 ${name} 的新价格`);
    if (!alive.current) return;
    setWorking(false);
    if (outcome.ok || outcome.uncertain) {onClose(); return;}
    setError(outcome.message); setConflict(!!outcome.conflict);
  };
  const reload = async () => {
    setWorking(true);
    try {await onReload();} finally {if (alive.current) {setWorking(false); setError(''); setConflict(false);}}
  };

  const sourceText = fields.pricedAs ? `按 ${fields.pricedAs} 的官方价` : !start ? '没有官方价：填四项官方价，或按其他模型定价'
    : start.from === 'version' ? '来自现行价格记录的官方价' : start.from === 'model' ? `来自官方价表 ${start.name}` : `来自官方价表 ${start.name}（上游模型）`;
  const costText = cost.costMultiplier === null ? '' : `${cost.basis ? `上游计费基准 ${cost.basis.map(usd => usdText(usd)).join(' / ')}` : '官方价'} × ${timesText(cost.costMultiplier)}（${cost.source === 'route' ? '这条线路' : cost.source === 'provider' ? '供应商' : '默认'}）`;
  return <Drawer id="price-drawer" label={`调价 · ${name}`} onClose={() => {if (!working) onClose();}} className="price-drawer">
    <header className="drawer-head">
      <div className="drawer-title"><span className="drawer-model">调价 · <span className="mono">{name}</span></span>
        <span className="muted">{nameList(chosen.flatMap(table => table.groups.map(group => String(group.name ?? group.id))))}</span></div>
      <div className="drawer-tools"><button type="button" className="btn-icon" aria-label="关闭调价" title="关闭（Esc）" disabled={working} onClick={onClose}><IconClose/></button></div>
    </header>
    <div className="drawer-body">
      {!tables.length && <p role="alert" className="form-error">这个模型的分组没有价格表，不能调价。</p>}
      {tables.length > 1 ? <fieldset className="field table-choice"><legend className="field-label">价格表（同一价格表的分组一起按新价格扣费）</legend>
        {tables.map(table => <label key={table.rateCardId} className="check-field"><input type="checkbox" checked={ticked.includes(table.rateCardId)}
          onChange={event => setTicked(value => event.target.checked ? [...value, table.rateCardId] : value.filter(id => id !== table.rateCardId))}/>{table.name}：{nameList(table.groups.map(group => String(group.name ?? group.id)))}</label>)}
      </fieldset> : first && <p className="note-info">{first.name}：{nameList(first.groups.map(group => String(group.name ?? group.id)))} 都按这个价格扣费。</p>}
      {chosen.flatMap(table => table.scheduled.map(version => <div key={String(version.id)} className="note-info scheduled-price">
        <span>已排期：{formatFullDateTime(Number(version.effective_from_secs))} 起 · 输入 {creditsText(version.fixed_input_credit_per_m) ?? '—'} / 输出 {creditsText(version.fixed_output_credit_per_m) ?? '—'} 积分/百万（{String(version.id)}）</span>
        <button type="button" className="btn btn-small" disabled={working} onClick={() => void cancel(version)}>取消排期</button></div>))}
      <fieldset disabled={working} className="price-form">
        <section aria-label="官方价" className="drawer-section">
          <h4>{mode === 'official' ? <>按官方价定价 <span className="muted">美元 / 百万 Tokens</span></> : '售价'}</h4>
          {mode === 'official' && <OfficialPriceFields value={fields} onChange={setFields} settings={settings} source={sourceText} costSource={costText} needsCost={cost.costMultiplier === null} results={results}/>}
          {mode === 'official' && newProblem && <p className="field-warning">{newProblem}</p>}
          <label className="check-field legacy-switch"><input type="checkbox" checked={mode === 'legacy'} onChange={event => setMode(event.target.checked ? 'legacy' : 'official')}/>高级：直接填积分（旧版，不记官方价）</label>
        </section>
        {mode === 'legacy' && <section aria-label="直接填积分" className="drawer-section legacy-price">
          <p className="note-warning">旧版定价：积分直接填，不记官方价；改积分面值时它的积分不会跟着重算。</p>
          <div className="table-scroll"><table className="table table-compact price-compare">
            <thead><tr><th/>{PRICE_FIELDS.map(([field, label]) => <th key={field} className="num">{label}</th>)}</tr></thead>
            <tbody>
              <tr><th scope="row">当前</th>{PRICE_FIELDS.map(([field]) => <td key={field} className="num">{current ? creditsText(current[field]) ?? '—' : '—'}</td>)}</tr>
              <tr><th scope="row">新价格</th>{PRICE_FIELDS.map(([field, label]) => <td key={field} className="num">
                <input aria-label={`新${label}售价`} inputMode="decimal" value={prices[field] ?? ''} onChange={event => setPrices({...prices, [field]: event.target.value})}/></td>)}</tr>
              <tr className="price-change"><th scope="row">变化</th>{PRICE_FIELDS.map(([field]) => {
                let after: number | null = null;
                try {after = priceToMicroPerMillion((prices[field] ?? '').trim(), 'million');} catch {/* shown as blank until valid */}
                const change = percentChange(current && typeof current[field] === 'number' ? Number(current[field]) : null, after);
                return <td key={field} className={`num${change.startsWith('+') ? ' is-up' : change.startsWith('−') ? ' is-down' : ''}`}>{change}</td>;
              })}</tr>
            </tbody>
          </table></div>
          <div className="listing-grid">
            {COST_FIELDS.map(([field, label]) => <label key={field} className="field"><span className="field-label">采购{label} ¥/百万</span>
              <input aria-label={`采购${label}价`} type="number" min="0" step="any" value={costs[field] ?? ''} onChange={event => setCosts({...costs, [field]: event.target.value})}/></label>)}
            <label className="field"><span className="field-label">版本倍率<InfoTip text="1 表示不加倍；与分组倍率、模型倍率相乘"/></span>
              <span className="input-suffix"><input aria-label="版本倍率" inputMode="decimal" value={versionMultiplier} onChange={event => setVersionMultiplier(event.target.value)}/><span>×</span></span></label>
          </div>
          {current?.currency === 'USD' && <p className="muted">现行价格的采购价是美元，已按旧版汇率 {settings.legacyRate ?? '—'} 换成人民币。</p>}
        </section>}
        <section className="pricing-preview" aria-label="扣费示例">
          <div role="status" className={`pricing-result${!sampleNew ? ' pricing-result-error' : sampleNew.includes('毛利约 −') ? ' is-losing' : ''}`}>
            <p>{sampleLabel} → {sampleNew ? <strong>新价格：{sampleNew}</strong> : <span>填好价格后显示新价格的示例</span>}</p>
            {sampleNow && <p className="muted">当前价格：{sampleNow}</p>}
          </div>
          <details className="sample-tokens"><summary>示例用量</summary>
            <p className="muted">{initialSample.count ? `默认取这个模型近 ${initialSample.count} 次成功请求的中位数` : '没有近期请求：默认 1,000 输入 + 1,000 输出'}</p>
            <div className="listing-grid pricing-tokens">{KINDS.map((label, index) => <label key={label} className="field"><span className="field-label">{label} Tokens</span>
              <input inputMode="numeric" value={tokens[index]} onChange={event => setTokens(values => values.map((value, i) => i === index ? event.target.value : value))}/></label>)}</div>
          </details>
        </section>
        {chosen.some(table => table.priced) ? <div className="field"><span className="field-label">生效时间</span>
          <div className="segmented" role="radiogroup" aria-label="生效时间">
            <button type="button" role="radio" aria-checked={timing === 'soon'} onClick={() => setTiming('soon')}>尽快（约 1 分钟后）</button>
            <button type="button" role="radio" aria-checked={timing === 'custom'} onClick={() => setTiming('custom')}>定时</button>
          </div>
          {timing === 'custom' && <input type="datetime-local" aria-label="定时生效时间" value={customTime} onChange={event => setCustomTime(event.target.value)}/>}
        </div> : <p className="muted">这是它在价格表里的第一个价格：发布即生效。</p>}
        {previous && <p className="restore-line"><button type="button" className="btn-text btn-small" onClick={restore}>恢复上一版价格</button>
          <span className="muted">{formatDateTime(Number(previous.effective_from_secs))} 起的那一版{officialOf(previous) ? `（官方价 × ${timesText(officialOf(previous)!.priceMultiplier)}）` : '（旧版积分）'}；作为新版本发布，历史不变</span></p>}
        <label className="field"><span className="field-label">原因<span className="required-mark">（必填）</span></span>
          <input aria-label="调价原因" maxLength={500} placeholder="例：按官网新价" value={reason} onChange={event => setReason(event.target.value)}/></label>
      </fieldset>
      {error && <div role="alert" className="form-error">
        <p>{error}</p>
        {conflict && <button type="button" className="btn btn-small" disabled={working} onClick={() => void reload()}>重新加载</button>}
      </div>}
    </div>
    <footer className="drawer-foot">
      <span className="muted">{current ? <>基于 <Tag>{String(current.id)}</Tag></> : '还没有价格，将新建'}</span>
      <span className="drawer-foot-spacer"/>
      <button type="button" className="btn" disabled={working} onClick={onClose}>取消</button>
      <button type="button" className="btn btn-primary" disabled={!!blocked} title={blocked} onClick={open}>{working ? '发布中…' : '预览调价'}</button>
    </footer>
    {preview && <PricingPreview plan={preview} providers={providers} groups={config.groups} onCancel={() => setPreview(null)} onConfirm={() => void confirm()}/>}
  </Drawer>;
}
