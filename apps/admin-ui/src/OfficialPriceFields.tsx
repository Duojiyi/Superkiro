// The official-price block 调价 and 上架 share: the four official USD prices (from the price in
// force, the official price table, or another model's — 按其他模型定价), the 计费倍率, and what the
// primary route costs as billing will cost it. The results table shows, per kind, our ¥ price,
// credits now → new and the route's cost, all from the one formula.
import {InfoTip} from './components/ui';
import {KINDS, type Four, type PricingSettings} from './officialPricing';
import {creditsText, percentChange} from './priceChange';
import {timesText, usdText, yuanText} from './pricingText';

export interface OfficialFieldsValue {
  usd: string[];
  /** The official price table entry the prices were copied from (按其他模型定价), if any. */
  pricedAs: string | null;
  priceMultiplier: string;
  /** Only when neither the route, its provider nor the default has a 成本倍率. */
  costMultiplier: string;
}

export default function OfficialPriceFields({value, onChange, settings, source, costSource, needsCost, results}: {
  value: OfficialFieldsValue;
  onChange: (value: OfficialFieldsValue) => void;
  settings: PricingSettings;
  /** Where the prices started from, in words. */
  source: string;
  /** The primary route's 成本倍率 and basis, in words; empty when it has to be typed here. */
  costSource: string;
  needsCost: boolean;
  /** Per kind: our ¥/M, credits now and new (micro), the primary route's cost ¥/M. */
  results: {yuan: Four; before: Four | null; after: Four; cost: Four | null} | null;
}) {
  const set = (patch: Partial<OfficialFieldsValue>) => onChange({...value, ...patch});
  const copy = (name: string) => {const price = settings.official[name]; if (price) set({usd: price.usd.map(String), pricedAs: name});};
  return <div className="official-fields">
    <div className="official-source"><span className="muted">{source}</span>
      <label className="priced-as"><span className="field-label">按其他模型定价</span>
        <select aria-label="按其他模型定价" value={value.pricedAs ?? ''} onChange={event => event.target.value ? copy(event.target.value) : set({pricedAs: null})}>
          <option value="">不按其他模型</option>
          {Object.keys(settings.official).sort().map(name => <option key={name} value={name}>{name}（{settings.official[name].usd.map(usd => usdText(usd)).join(' / ')}）</option>)}
        </select></label></div>
    <div className="listing-grid">
      {KINDS.map((kind, index) => <label key={kind} className="field"><span className="field-label">官方{kind} ${kind === '缓存写' && <InfoTip text="5 分钟缓存的写入价"/>}</span>
        <input aria-label={`官方${kind}价`} inputMode="decimal" value={value.usd[index]} onChange={event => set({usd: value.usd.map((item, i) => i === index ? event.target.value : item), pricedAs: null})}/></label>)}
      <label className="field"><span className="field-label">计费倍率<InfoTip text="我们的价格 = 官方价 × 计费倍率 × 官方价 $1 折合的人民币；默认值在“定价设置”"/></span>
        <span className="input-suffix"><input aria-label="计费倍率" inputMode="decimal" placeholder={settings.defaultPrice !== null ? `默认 ${settings.defaultPrice}` : '如 0.24'} value={value.priceMultiplier} onChange={event => set({priceMultiplier: event.target.value})}/><span>×</span></span></label>
      {needsCost ? <label className="field"><span className="field-label">成本倍率<InfoTip text="主线路的供应商和默认都没有成本倍率：先在这里填；以后在“定价设置”里给供应商设"/></span>
        <span className="input-suffix"><input aria-label="成本倍率" inputMode="decimal" placeholder="如 0.08" value={value.costMultiplier} onChange={event => set({costMultiplier: event.target.value})}/><span>×</span></span></label>
        : <div className="field official-cost"><span className="field-label">成本（主线路）</span><span className="muted">{costSource}</span></div>}
    </div>
    {results && <div className="table-scroll"><table className="table table-compact price-compare official-results" aria-label="按官方价算出的价格">
      <thead><tr><th/>{KINDS.map(kind => <th key={kind} className="num">{kind}</th>)}</tr></thead>
      <tbody>
        <tr><th scope="row">我们的价 ¥/百万</th>{results.yuan.map((yuan, index) => <td key={index} className="num">{yuanText(yuan)}</td>)}</tr>
        <tr><th scope="row">积分/百万</th>{results.after.map((credits, index) => {
          const before = results.before?.[index] ?? null, change = percentChange(before, credits);
          return <td key={index} className="num">{before !== null && before !== credits ? <>{creditsText(before)} → </> : null}<b>{creditsText(credits)}</b>
            {before !== null && before !== credits && <span className={change.startsWith('+') ? ' is-up' : ' is-down'}> {change}</span>}</td>;
        })}</tr>
        <tr><th scope="row">主线路成本 ¥/百万</th>{KINDS.map((kind, index) => <td key={kind} className={`num${results.cost && results.cost[index] > 0 && results.cost[index] >= results.yuan[index] ? ' is-loss' : ''}`}>{results.cost ? yuanText(results.cost[index]) : '—'}</td>)}</tr>
      </tbody>
    </table></div>}
    {value.pricedAs && <p className="muted">按 {value.pricedAs} 的官方价定价（{timesText(Number(value.priceMultiplier) || null)}）：发布原因里会记下“按 {value.pricedAs} 定价”。</p>}
  </div>;
}
