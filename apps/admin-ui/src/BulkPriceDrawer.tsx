// 批量调价: new prices for the chosen models in one publication, one version each, all from the
// same time. Each model's price in force is × a factor, ± a percentage, or computed from its
// official USD prices × the retail multiplier; the preview shows old → new credits and the margin
// on a sample request before anything is sent. Procurement prices and version multipliers stay
// as they are.
import {useState} from 'react';
import {adminApi, type CommercialConfig} from './api';
import {confirmAction} from './components/confirm';
import {IconClose} from './components/icons';
import {Drawer} from './components/modal';
import {toast} from './components/toast';
import {InfoTip} from './components/ui';
import {formatFullDateTime} from './format';
import {creditsFromOfficial, sharedPrice} from './listing';
import {buildPriceVersion, COST_FIELDS, creditsText, currentVersion, percentChange, PRICE_FIELDS, sampleCost, scaledPrice, versionIdFor} from './priceChange';
import {formatMicroPrice} from './pricing';
import type {PublishOutcome} from './refusal';
import {loadOfficial, loadRates, saveOfficial, saveRates} from './remembered';
import {modelName, nameList} from './routes';

type Row = Record<string, unknown>;
type Mode = 'factor' | 'percent' | 'official';
/** One chosen model: priced with another one (sameAs), or its price now and new, or why there is none. */
interface Plan {model: Row; sameAs?: Row; rateCardId?: string; current?: Row | null; others?: Row[]; before?: number[]; after?: number[]; marginBefore?: number | null; marginAfter?: number | null; problem?: string}

const SOON_SECS = 5 * 60;
const soon = () => Math.ceil((adminApi.serverNowMs / 1000 + SOON_SECS) / 60) * 60;
const toLocalInput = (secs: number) => {
  const date = new Date(secs * 1000);
  return new Date(date.getTime() - date.getTimezoneOffset() * 60000).toISOString().slice(0, 16);
};
const validReason = (text: string) => !!text.trim() && new TextEncoder().encode(text.trim()).length <= 500 && !/[\x00-\x1f\x7f-\x9f]/.test(text);
const OFFICIAL_LABELS = ['输入', '输出', '缓存写', '缓存读'];
const margin = (value: number | null) => value === null ? '—' : `${Math.round(value)}%`;

export default function BulkPriceDrawer({models, config, onClose, onPublish}: {
  /** The chosen models, as published. */
  models: Row[];
  config: CommercialConfig;
  onClose: () => void;
  onPublish: (update: {versions: Row[]}, reason: string, check: string) => Promise<PublishOutcome>;
}) {
  const [mode, setMode] = useState<Mode>('percent');
  const [factor, setFactor] = useState('');
  const [percent, setPercent] = useState('');
  const [rates, setRates] = useState(loadRates);
  const [official, setOfficial] = useState<Record<string, string[]>>(() => Object.fromEntries(models.map(model => [String(model.exposed_model_id), loadOfficial(String(model.exposed_model_id))])));
  const [timing, setTiming] = useState<'soon' | 'custom'>('soon');
  const [customTime, setCustomTime] = useState(() => toLocalInput(soon()));
  const [reason, setReason] = useState('');
  const [error, setError] = useState('');
  const [working, setWorking] = useState(false);
  const nowSecs = adminApi.serverNowMs / 1000, settings = config.settings;
  const effectiveSecs = timing === 'soon' ? soon() : Math.floor(new Date(customTime).getTime() / 1000);
  const name = (model: Row) => modelName(model, config.models, config.groups);

  // One price per price table and model ID: a model sharing another chosen one's price is priced with it.
  const seen = new Map<string, Row>();
  const plan: Plan[] = models.map((model): Plan => {
    const group = config.groups.find(item => item.id === model.group_id), rateCardId = String(group?.rate_card_id ?? ''), id = String(model.exposed_model_id);
    const key = `${rateCardId}\n${id}`, sameAs = seen.get(key);
    if (sameAs) return {model, sameAs};
    seen.set(key, model);
    const current = currentVersion(config.versions, rateCardId, [model.exposed_model_id, model.target_model], nowSecs);
    const others = sharedPrice(config, model.group_id, id).groups.filter(item => !models.some(chosen => chosen.group_id === item.id && chosen.exposed_model_id === id));
    const sample = (rates: number[]) => {try {return sampleCost({rates, tokens: ['1000', '1000', '0', '0'], multipliers: [Number(current?.margin_multiplier ?? 1), Number(group?.margin_multiplier ?? 1), Number(model.credit_multiplier ?? 1)],
      faceValueCny: settings?.credit_face_value_cny, costs: COST_FIELDS.map(([field]) => Number(current?.[field] ?? NaN)), currency: current?.currency, usdCnyRate: settings?.usd_cny_rate}).marginPct;} catch {return null;}};
    if (!current || current.pricing_mode !== 'fixed') return {model, rateCardId, current, others, problem: current ? '不是固定价格：请单独调价' : '还没有价格：请单独调价'};
    const before = PRICE_FIELDS.map(([field]) => Number(current[field]));
    try {
      const after = mode === 'official' ? PRICE_FIELDS.map((_, index) => {
        const typed = official[id]?.[index] ?? '';
        if (!typed.trim()) throw new Error('填写四项官方价（美元 / 百万 Tokens，免费填 0）');
        return creditsFromOfficial(typed, rates.retail, settings?.credit_face_value_cny);
      }) : PRICE_FIELDS.map(([field]) => scaledPrice(Number(current[field]), mode, mode === 'factor' ? factor : percent));
      return {model, rateCardId, current, others, before, after, marginBefore: sample(before), marginAfter: sample(after)};
    } catch (cause) {return {model, rateCardId, current, others, before, problem: cause instanceof Error ? cause.message : String(cause)};}
  });
  const priced = plan.filter(entry => !entry.sameAs && entry.after);
  const problems = plan.filter(entry => entry.problem);
  const typed = mode === 'factor' ? factor.trim() : mode === 'percent' ? percent.trim() : rates.retail.trim();
  const blocked = working ? '正在发布' : !typed ? (mode === 'official' ? '填写售价倍率' : mode === 'factor' ? '填写系数' : '填写百分比') : problems.length ? '有模型算不出新价格，见表格'
    : !validReason(reason) ? '填写原因后可发布（最多约 160 字）' : undefined;

  const submit = async () => {
    if (blocked) return;
    setError('');
    const when = timing === 'soon' ? soon() : effectiveSecs;
    let versions: Row[];
    try {
      if (!Number.isSafeInteger(when) || when <= adminApi.serverNowMs / 1000) throw new Error('生效时间需晚于现在');
      const taken = config.versions.map(version => version.id);
      versions = priced.map(entry => {
        const version = buildPriceVersion({prices: Object.fromEntries(PRICE_FIELDS.map(([field], index) => [field, formatMicroPrice(entry.after![index])])),
          costs: Object.fromEntries(COST_FIELDS.map(([field]) => [field, typeof entry.current![field] === 'number' ? String(entry.current![field]) : ''])),
          currency: String(entry.current!.currency ?? ''), multiplier: String(entry.current!.margin_multiplier ?? 1), effectiveSecs: when,
          id: versionIdFor(String(entry.model.exposed_model_id), when, taken)},
        {model: String(entry.model.exposed_model_id), rateCardId: entry.rateCardId!, versions: config.versions, nowSecs: adminApi.serverNowMs / 1000, base: entry.current});
        taken.push(version.id);
        return version;
      });
    } catch (cause) {
      const text = cause instanceof Error ? cause.message : String(cause);
      setError(text.includes('采购') ? `${text}：这个模型的现价缺少采购价，请先单独调价补上` : text); return;
    }
    const shared = priced.flatMap(entry => entry.others!.map(group => `${String(group.name ?? group.id)} 的 ${String(entry.model.exposed_model_id)}`));
    const confirmed = await confirmAction({
      title: `发布 ${versions.length} 个模型的新价格？`,
      facts: [...priced.slice(0, 8).map(entry => `${name(entry.model)}：输入 ${creditsText(entry.before![0])} → ${formatMicroPrice(entry.after![0])}，输出 ${creditsText(entry.before![1])} → ${formatMicroPrice(entry.after![1])} 积分/百万`),
        ...(priced.length > 8 ? [`等 ${priced.length} 个`] : []),
        ...(shared.length ? [`同一价格表，同时生效于：${nameList(shared)}`] : []),
        `生效：${formatFullDateTime(when)}`, `原因：${reason.trim()}`],
      consequence: '到生效时间后，新请求按新价格扣费；已开始的请求按原价格结算。采购价不变。',
      confirmLabel: '发布调价',
    });
    if (!confirmed) return;
    if (when <= adminApi.serverNowMs / 1000) {setError('生效时间已过，请重新选择'); return;}
    if (mode === 'official') {saveRates(rates); for (const [id, prices] of Object.entries(official)) if (prices.every(value => value.trim())) saveOfficial(id, prices);}
    setWorking(true);
    const outcome = await onPublish({versions}, reason.trim(), `“价格版本”里这些模型有没有 ${formatFullDateTime(when)} 起的新价格：${nameList(priced.map(entry => name(entry.model)))}`);
    setWorking(false);
    if (outcome.ok) {toast.success(`已发布 ${versions.length} 个模型的新价格`); onClose(); return;}
    if (outcome.uncertain) {onClose(); return;}
    setError(outcome.message);
  };

  return <Drawer id="bulk-price" label="批量调价" onClose={() => {if (!working) onClose();}} className="price-drawer switch-drawer">
    <header className="drawer-head">
      <div className="drawer-title"><span className="drawer-model">批量调价</span><span className="muted">{models.length} 个模型</span></div>
      <div className="drawer-tools"><button type="button" className="btn-icon" aria-label="关闭批量调价" title="关闭（Esc）" disabled={working} onClick={onClose}><IconClose/></button></div>
    </header>
    <div className="drawer-body">
      <fieldset disabled={working} className="price-form">
        <div className="form-grid form-grid-2">
          <div className="field"><span className="field-label">怎么调</span>
            <div className="segmented" role="radiogroup" aria-label="调价方式">
              {([['percent', '± 百分比'], ['factor', '× 系数'], ['official', '官方价 × 售价倍率']] as Array<[Mode, string]>).map(([value, label]) =>
                <button key={value} type="button" role="radio" aria-checked={mode === value} onClick={() => setMode(value)}>{label}</button>)}
            </div></div>
          {mode === 'percent' && <label className="field"><span className="field-label">百分比<InfoTip text="正数涨价、负数降价，如 -10 表示降 10%"/></span>
            <span className="input-suffix"><input aria-label="调价百分比" inputMode="decimal" placeholder="如 -10" value={percent} onChange={event => setPercent(event.target.value)}/><span>%</span></span></label>}
          {mode === 'factor' && <label className="field"><span className="field-label">系数<InfoTip text="新价格 = 现价 × 系数，如 1.2"/></span>
            <span className="input-suffix"><input aria-label="调价系数" inputMode="decimal" placeholder="如 1.2" value={factor} onChange={event => setFactor(event.target.value)}/><span>×</span></span></label>}
          {mode === 'official' && <label className="field"><span className="field-label">售价倍率<InfoTip text="客户每用官方 1 美元，花多少元（如 0.24）；按积分面值换成积分"/></span>
            <input aria-label="售价倍率" inputMode="decimal" placeholder="如 0.24" value={rates.retail} onChange={event => setRates({...rates, retail: event.target.value})}/></label>}
        </div>
        <div className="table-scroll"><table className="table table-compact switch-preview bulk-preview">
          <thead><tr><th>模型</th>{mode === 'official' && <th>官方价 $ / 百万（入 / 出 / 缓存写 / 缓存读）</th>}<th className="num">现价（入 / 出）</th><th className="num">新价（入 / 出）</th><th className="num">变化</th><th className="num">示例毛利</th></tr></thead>
          <tbody>{plan.map(entry => {
            const id = String(entry.model.exposed_model_id), key = String(entry.model.id);
            if (entry.sameAs) return <tr key={key} className="is-muted"><td className="mono">{name(entry.model)}</td><td colSpan={mode === 'official' ? 5 : 4}>和 {name(entry.sameAs!)} 用同一个价格，一起调</td></tr>;
            const before = entry.before, after = entry.after;
            return <tr key={key}>
              <td className="mono">{name(entry.model)}{entry.others!.length > 0 && <span className="field-hint"> 同价：{entry.others!.map(group => String(group.name ?? group.id)).join('、')}</span>}</td>
              {mode === 'official' && <td><span className="switch-cost-form">{OFFICIAL_LABELS.map((label, index) => <input key={label} aria-label={`${id} 官方${label}价`} placeholder={label} inputMode="decimal"
                value={official[id]?.[index] ?? ''} onChange={event => setOfficial({...official, [id]: (official[id] ?? ['', '', '', '']).map((value, i) => i === index ? event.target.value : value)})}/>)}</span></td>}
              <td className="num">{before ? `${creditsText(before[0])} / ${creditsText(before[1])}` : '—'}</td>
              <td className="num">{after ? <b>{formatMicroPrice(after[0])} / {formatMicroPrice(after[1])}</b> : <span className="field-warning">{entry.problem}</span>}</td>
              <td className="num">{before && after ? `${percentChange(before[0], after[0])} / ${percentChange(before[1], after[1])}` : ''}</td>
              <td className="num">{after ? `${margin(entry.marginBefore ?? null)} → ${margin(entry.marginAfter ?? null)}` : ''}</td>
            </tr>;
          })}</tbody>
        </table></div>
        <p className="muted">示例：1,000 输入 + 1,000 输出 Tokens；缓存写、缓存读按同样方式调整。采购价和版本倍率不变。</p>
        <div className="field"><span className="field-label">生效时间</span>
          <div className="segmented" role="radiogroup" aria-label="生效时间">
            <button type="button" role="radio" aria-checked={timing === 'soon'} onClick={() => setTiming('soon')}>发布后 5 分钟</button>
            <button type="button" role="radio" aria-checked={timing === 'custom'} onClick={() => setTiming('custom')}>自定义</button>
          </div>
          {timing === 'custom' && <input type="datetime-local" aria-label="自定义生效时间" value={customTime} onChange={event => setCustomTime(event.target.value)}/>}
        </div>
        <label className="field"><span className="field-label">原因<span className="required-mark">（必填）</span></span>
          <input aria-label="批量调价原因" maxLength={500} placeholder="例：输出价统一下调 10%" value={reason} onChange={event => setReason(event.target.value)}/></label>
      </fieldset>
      {error && <div role="alert" className="form-error"><p>{error}</p></div>}
    </div>
    <footer className="drawer-foot">
      <span className="muted">一次发布，每个模型一个新价格版本</span>
      <span className="drawer-foot-spacer"/>
      <button type="button" className="btn" disabled={working} onClick={onClose}>取消</button>
      <button type="button" className="btn btn-primary" disabled={!!blocked} title={blocked} onClick={() => void submit()}>{working ? '发布中…' : '发布调价'}</button>
    </footer>
  </Drawer>;
}
