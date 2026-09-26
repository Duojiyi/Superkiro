// 调价: one drawer, one step. Current and new prices side by side (typed, or computed from the
// official price and the multipliers), what a sample request costs and earns, when it takes
// effect, a reason, then 发布调价 (confirmed) — published on its own against the configuration
// version read, like every other change.
import {useEffect, useRef, useState} from 'react';
import type {CommercialConfig} from './api';
import {confirmAction} from './components/confirm';
import {IconClose} from './components/icons';
import {Drawer} from './components/modal';
import {InfoTip, Tag} from './components/ui';
import {formatCount, formatFullDateTime, shortHash} from './format';
import {costFromOfficial, creditsFromOfficial} from './listing';
import {buildPriceVersion, COST_FIELDS, creditsText, currentVersion, percentChange, PRICE_FIELDS, sampleCost, scheduledVersions, versionIdFor} from './priceChange';
import {formatMicroPrice, priceToMicroPerMillion} from './pricing';
import type {PublishOutcome} from './refusal';
import {loadOfficial, loadRates, saveOfficial, saveRates} from './remembered';

type Row = Record<string, unknown>;
export type {PublishOutcome};

const SOON_SECS = 5 * 60;
const soon = () => Math.ceil((Date.now() / 1000 + SOON_SECS) / 60) * 60;
const toLocalInput = (secs: number) => {
  const date = new Date(secs * 1000);
  return new Date(date.getTime() - date.getTimezoneOffset() * 60000).toISOString().slice(0, 16);
};
const yuan = (value: number | null) => value === null ? '—' : `¥${value < 0.01 ? value.toFixed(4) : value.toFixed(2)}`;
const OFFICIAL_LABELS = ['输入', '输出', '缓存写', '缓存读'];
const validReason = (text: string) => !!text.trim() && new TextEncoder().encode(text.trim()).length <= 500 && !/[\x00-\x1f\x7f-\x9f]/.test(text);

export default function PriceDrawer({model, group, config, onClose, onPublish, onReload}: {
  model: Row;
  group: Row | null;
  config: CommercialConfig;
  onClose: () => void;
  onPublish: (version: Row, reason: string) => Promise<PublishOutcome>;
  onReload: () => Promise<void>;
}) {
  const name = String(model.exposed_model_id ?? model.id);
  const names = [model.exposed_model_id, model.target_model];
  const rateCardId = typeof group?.rate_card_id === 'string' ? group.rate_card_id : '';
  const nowSecs = Date.now() / 1000;
  const current = rateCardId ? currentVersion(config.versions, rateCardId, names, nowSecs) : null;
  const scheduled = rateCardId ? scheduledVersions(config.versions, rateCardId, names, nowSecs) : [];
  const [prices, setPrices] = useState<Record<string, string>>(() => Object.fromEntries(PRICE_FIELDS.map(([field]) => [field, creditsText(current?.[field]) ?? ''])));
  const [costs, setCosts] = useState<Record<string, string>>(() => Object.fromEntries(COST_FIELDS.map(([field]) => [field, typeof current?.[field] === 'number' ? String(current[field]) : ''])));
  const [currency, setCurrency] = useState(() => ['USD', 'CNY'].includes(String(current?.currency)) ? String(current!.currency) : 'USD');
  const [multiplier, setMultiplier] = useState(() => String(current?.margin_multiplier ?? 1));
  const [timing, setTiming] = useState<'soon' | 'custom'>('soon');
  const [customTime, setCustomTime] = useState(() => toLocalInput(soon()));
  const [idText, setIdText] = useState<string | null>(null);
  const [reason, setReason] = useState('');
  const [tokens, setTokens] = useState(['1000', '1000', '0', '0']);
  const [error, setError] = useState('');
  const [conflict, setConflict] = useState(false);
  const [working, setWorking] = useState(false);
  // Procurement prices are folded away unless the current version has none (then they are needed).
  const [costsOpen, setCostsOpen] = useState(() => !current || COST_FIELDS.some(([field]) => typeof current[field] !== 'number'));
  // 按官方价计算: the model's official prices and the multipliers typed before, for this provider.
  const providerId = String(model.target_provider_id ?? '');
  const [official, setOfficial] = useState(() => loadOfficial(name));
  const [rates, setRates] = useState(loadRates);
  const upstreamRate = rates.upstream[providerId] ?? '';
  const alive = useRef(true);
  useEffect(() => () => {alive.current = false;}, []);

  const effectiveSecs = timing === 'soon' ? soon() : Math.floor(new Date(customTime).getTime() / 1000);
  const generatedId = versionIdFor(String(model.exposed_model_id ?? model.id), Number.isFinite(effectiveSecs) ? effectiveSecs : soon(), config.versions.map(version => version.id));
  const versionId = idText ?? generatedId;
  const settings = config.settings;
  // The customer's charge follows billing exactly (pricing.ts); ¥ is an estimate at the face value.
  const sample = (rates: number[], costValues: number[], money: string, versionMultiplier: number) => sampleCost({rates, tokens,
    multipliers: [versionMultiplier, Number(group?.margin_multiplier ?? 1), Number(model.credit_multiplier ?? 1)],
    faceValueCny: settings?.credit_face_value_cny, costs: costValues, currency: money, usdCnyRate: settings?.usd_cny_rate});
  let preview = '', previewNow = '', previewError = '', losing = false;
  try {
    if (!multiplier.trim()) throw new Error('请填写版本倍率');
    const rates = PRICE_FIELDS.map(([field]) => priceToMicroPerMillion((prices[field] ?? '').trim(), 'million'));
    const result = sample(rates, COST_FIELDS.map(([field]) => Number((costs[field] ?? '').trim() || NaN)), currency, Number(multiplier));
    preview = `${formatMicroPrice(result.credits)} 积分（≈ ${yuan(result.yuan)}）· 采购 ≈ ${yuan(result.costYuan)} · 毛利${result.marginPct === null ? '暂不计算' : `约 ${Math.round(result.marginPct)}%`}`;
    losing = result.marginPct !== null && result.marginPct < 0;
  } catch (cause) {previewError = cause instanceof Error ? cause.message : String(cause);}
  if (current && current.pricing_mode === 'fixed') {
    try {
      const result = sample(PRICE_FIELDS.map(([field]) => Number(current[field])), COST_FIELDS.map(([field]) => Number(current[field])), String(current.currency), Number(current.margin_multiplier ?? 1));
      previewNow = `当前价格：${formatMicroPrice(result.credits)} 积分（≈ ${yuan(result.yuan)}）· 毛利${result.marginPct === null ? '暂不计算' : `约 ${Math.round(result.marginPct)}%`}`;
    } catch {/* the current price cannot be shown safely; only the new one is previewed */}
  }
  const sampleLabel = `示例 ${formatCount(Number(tokens[0]) || 0)} 输入 + ${formatCount(Number(tokens[1]) || 0)} 输出${Number(tokens[2]) ? ` + ${formatCount(Number(tokens[2]))} 缓存写` : ''}${Number(tokens[3]) ? ` + ${formatCount(Number(tokens[3]))} 缓存读` : ''}`;
  const chain = `用量费用 × 版本 ${multiplier || '—'} × 分组 ${String(group?.margin_multiplier ?? '—')} × 模型 ${String(model.credit_multiplier ?? '—')}，向上取整到 1 微积分；¥ 按积分面值 ${String(settings?.credit_face_value_cny ?? '—')} 元，USD 采购价按汇率 ${String(settings?.usd_cny_rate ?? '—')}`;
  const blocked = !rateCardId ? '这个模型的分组没有价格表，不能调价' : !validReason(reason) ? '填写原因后可发布（最多约 160 字）' : undefined;
  // New prices from the official ones: retail multiplier × official at the face value; costs from this upstream's multiplier.
  const compute = () => {
    setError('');
    try {
      if (official.some(value => !value.trim())) throw new Error('先填四项官方价（美元 / 百万 Tokens，免费填 0）');
      if (!rates.retail.trim() && !upstreamRate.trim()) throw new Error('填写售价倍率或成本倍率（至少一项）');
      if (rates.retail.trim()) setPrices(Object.fromEntries(PRICE_FIELDS.map(([field], index) => [field, formatMicroPrice(creditsFromOfficial(official[index], rates.retail, settings?.credit_face_value_cny))])));
      if (upstreamRate.trim()) {setCosts(Object.fromEntries(COST_FIELDS.map(([field], index) => [field, String(costFromOfficial(official[index], upstreamRate))]))); setCurrency('CNY');}
      saveRates(rates); saveOfficial(name, official);
    } catch (cause) {setError(cause instanceof Error ? cause.message : String(cause));}
  };

  const submit = async () => {
    if (working || blocked) return;
    setError(''); setConflict(false);
    const now = Date.now() / 1000;
    const when = timing === 'soon' ? soon() : effectiveSecs;
    const id = idText ?? versionIdFor(name, when, config.versions.map(version => version.id));
    let version: Row;
    try {version = buildPriceVersion({prices, costs, currency, multiplier, effectiveSecs: when, id}, {model: name, rateCardId, versions: config.versions, nowSecs: now, base: current});}
    catch (cause) {
      const text = cause instanceof Error ? cause.message : String(cause);
      if (text.includes('采购')) setCostsOpen(true);
      setError(text); return;
    }
    const changes: string[] = [];
    for (const [field, label] of PRICE_FIELDS) {
      const before = current ? creditsText(current[field]) : null, after = creditsText(version[field]);
      if (before !== after) changes.push(`${label} ${before ?? '—'} → ${after} 积分/百万`);
    }
    for (const [field, label] of COST_FIELDS) {
      if (current?.[field] !== version[field]) changes.push(`采购${label} ${current?.[field] ?? '—'} → ${String(version[field])} ${currency}/百万`);
    }
    if (current && current.currency !== currency) changes.push(`采购币种 ${String(current.currency ?? '—')} → ${currency}`);
    if (Number(current?.margin_multiplier ?? NaN) !== version.margin_multiplier) changes.push(`版本倍率 ${String(current?.margin_multiplier ?? '—')} → ${String(version.margin_multiplier)}`);
    if (current && !changes.length) {setError('价格没有变化'); return;}
    const confirmed = await confirmAction({
      title: `发布 ${name} 的新价格？`,
      facts: [...changes, `生效：${formatFullDateTime(when)}`, `版本 ${id}`, `原因：${reason.trim()}`, `基于配置版本 ${shortHash(config.revision)}`],
      consequence: '到生效时间后，新请求按新价格扣费；已开始的请求按原价格结算。',
      confirmLabel: '发布调价',
    });
    if (!confirmed || !alive.current) return;
    if (when <= Date.now() / 1000) {setError('生效时间已过，请重新选择'); return;}
    setWorking(true);
    const outcome = await onPublish(version, reason.trim());
    if (!alive.current) return;
    setWorking(false);
    if (outcome.ok || outcome.uncertain) {onClose(); return;}
    setError(outcome.message); setConflict(!!outcome.conflict);
  };
  const reload = async () => {
    setWorking(true);
    try {await onReload();} finally {if (alive.current) {setWorking(false); setError(''); setConflict(false);}}
  };

  return <Drawer id="price-drawer" label={`调价 · ${name}`} onClose={() => {if (!working) onClose();}} className="price-drawer">
    <header className="drawer-head">
      <div className="drawer-title"><span className="drawer-model">调价 · <span className="mono">{name}</span></span>
        {group && <span className="muted">{String(group.name ?? group.id)}</span>}</div>
      <div className="drawer-tools"><button type="button" className="btn-icon" aria-label="关闭调价" title="关闭（Esc）" disabled={working} onClick={onClose}><IconClose/></button></div>
    </header>
    <div className="drawer-body">
      {!rateCardId && <p role="alert" className="form-error">这个模型的分组没有价格表，不能调价。</p>}
      {scheduled.length > 0 && <p className="note-info">已排期：{scheduled.map(version => `${formatFullDateTime(Number(version.effective_from_secs))} 起（${String(version.id)}）`).join('；')}</p>}
      <fieldset disabled={working} className="price-form">
        <section aria-label="售价">
          <h4>售价 · 积分 / 百万 Tokens</h4>
          <div className="table-scroll"><table className="table table-compact price-compare">
            <thead><tr><th/>{PRICE_FIELDS.map(([field, label]) => <th key={field} className="num">{label}</th>)}</tr></thead>
            <tbody>
              <tr><th scope="row">当前</th>{PRICE_FIELDS.map(([field]) => <td key={field} className="num">{current ? creditsText(current[field]) ?? '—' : '—'}</td>)}</tr>
              <tr><th scope="row">新价格</th>{PRICE_FIELDS.map(([field, label]) => <td key={field} className="num">
                <input aria-label={`新${label}售价`} inputMode="decimal" value={prices[field] ?? ''} onChange={event => setPrices({...prices, [field]: event.target.value})}/></td>)}</tr>
              <tr className="price-change"><th scope="row">变化</th>{PRICE_FIELDS.map(([field]) => {
                let after: number | null = null;
                try {after = priceToMicroPerMillion((prices[field] ?? '').trim(), 'million');} catch {/* shown as blank until valid */}
                const before = current && typeof current[field] === 'number' ? Number(current[field]) : null;
                const change = percentChange(before, after);
                return <td key={field} className={`num${change.startsWith('+') ? ' is-up' : change.startsWith('−') ? ' is-down' : ''}`}>{change}</td>;
              })}</tr>
            </tbody>
          </table></div>
        </section>
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
        <div className="form-grid form-grid-2">
          <label className="field"><span className="field-label">版本倍率<InfoTip text="1 表示不加倍；与分组、模型的倍率相乘"/></span>
            <span className="input-suffix"><input aria-label="版本倍率" inputMode="decimal" value={multiplier} onChange={event => setMultiplier(event.target.value)}/><span>×</span></span></label>
          <div className="field"><span className="field-label">生效时间</span>
            <div className="segmented" role="radiogroup" aria-label="生效时间">
              <button type="button" role="radio" aria-checked={timing === 'soon'} onClick={() => setTiming('soon')}>发布后 5 分钟</button>
              <button type="button" role="radio" aria-checked={timing === 'custom'} onClick={() => setTiming('custom')}>自定义</button>
            </div>
            {timing === 'custom' && <input type="datetime-local" aria-label="自定义生效时间" value={customTime} onChange={event => setCustomTime(event.target.value)}/>}
          </div>
        </div>
        <section className="pricing-preview" aria-label="扣费示例">
          <p role="status" className={`pricing-result${previewError ? ' pricing-result-error' : losing ? ' is-losing' : ''}`} title={previewError ? undefined : chain}>
            {previewError || <>{sampleLabel} → <strong>{preview}</strong></>}</p>
          {previewNow && !previewError && <p className="muted">{previewNow}</p>}
          <details className="sample-tokens"><summary>示例用量</summary>
            <div className="pricing-grid pricing-tokens">{['输入', '输出', '缓存写', '缓存读'].map((label, index) => <label key={label}>{label} Tokens
              <input inputMode="numeric" value={tokens[index]} onChange={event => setTokens(values => values.map((value, i) => i === index ? event.target.value : value))}/></label>)}</div>
          </details>
        </section>
        <details className="price-costs" open={costsOpen} onToggle={event => setCostsOpen(event.currentTarget.open)}>
          <summary>采购价 · {currency} / 百万 Tokens（用于成本估算）{current && <span className="muted"> 当前 {COST_FIELDS.map(([field]) => String(current[field] ?? '—')).join(' / ')}</span>}</summary>
          <div className="pricing-grid">
            <label>币种<select aria-label="采购价币种" value={currency} onChange={event => setCurrency(event.target.value)}><option value="USD">USD</option><option value="CNY">CNY</option></select></label>
            {COST_FIELDS.map(([field, label]) => <label key={field}>{label}<input aria-label={`采购${label}价`} type="number" min="0" step="any" value={costs[field] ?? ''} onChange={event => setCosts({...costs, [field]: event.target.value})}/></label>)}
          </div>
        </details>
        <details className="price-advanced">
          <summary>高级：版本 ID <span className="mono muted">{versionId}</span></summary>
          <label className="field"><span className="field-label">版本 ID</span>
            <input aria-label="版本 ID" value={versionId} onChange={event => setIdText(event.target.value)}/></label>
          {idText !== null && <button type="button" className="btn-text" onClick={() => setIdText(null)}>恢复自动生成</button>}
        </details>
        <label className="field"><span className="field-label">原因<span className="required-mark">（必填）</span></span>
          <input aria-label="调价原因" maxLength={500} placeholder="例：输出价下调 20%" value={reason} onChange={event => setReason(event.target.value)}/></label>
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
      <button type="button" className="btn btn-primary" disabled={working || !!blocked} title={blocked} onClick={() => void submit()}>{working ? '发布中…' : '发布调价'}</button>
    </footer>
  </Drawer>;
}
