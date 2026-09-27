// 批量调价: new prices for the chosen models in one publication, one version per price table and
// model, all from the same time, each computed from an official price: 计费倍率 set to one value,
// the official prices moved ± a percentage, or everything re-derived at the current settings (the
// official price table, the route's 成本倍率, the face value). A model priced the old way (credits
// typed, no official price) is priced from the official price table when it has an entry, and
// named; without one it is left out. Every change ends in the preview, then one publication.
import {useState} from 'react';
import {adminApi, type CommercialConfig} from './api';
import {IconClose} from './components/icons';
import {Drawer} from './components/modal';
import {toast} from './components/toast';
import {InfoTip} from './components/ui';
import {formatFullDateTime} from './format';
import {creditsOf, creditsOfVersion, freeTime, officialOf, officialProblem, officialStart, officialVersion, pricingImpact, primaryCost, readSettings, type Four, type OfficialInput} from './officialPricing';
import {soonSecs} from './PriceDrawer';
import {creditsText, currentVersion, percentChange, versionIdFor} from './priceChange';
import PricingPreview, {type PreviewPlan} from './PricingPreview';
import {marginText, marginTone, timesText, typedNumber, usdText} from './pricingText';
import type {PublishOutcome} from './refusal';
import {modelName, nameList} from './routes';

type Row = Record<string, unknown>;
type Mode = 'multiplier' | 'scale' | 'rederive';
/** One price to change: a model in one price table (the chosen entries it charges), now and new. */
interface Entry {
  key: string; rateCardId: string; model: string; mappings: Row[]; current: Row | null;
  /** Priced the old way: its new price comes from the official price table. */
  legacy: boolean;
  next?: OfficialInput; problem?: string;
}

const toLocalInput = (secs: number) => {
  const date = new Date(secs * 1000);
  return new Date(date.getTime() - date.getTimezoneOffset() * 60000).toISOString().slice(0, 16);
};
const validReason = (text: string) => !!text.trim() && new TextEncoder().encode(text.trim()).length <= 500 && !/[\x00-\x1f\x7f-\x9f]/.test(text);
const MODES: Array<[Mode, string]> = [['multiplier', '设计费倍率'], ['scale', '官方价 ± %'], ['rederive', '按当前设置重算']];

export default function BulkPriceDrawer({models, config, providers, sampleOf, onClose, onPublish}: {
  /** The chosen entries, as published. */
  models: Row[];
  config: CommercialConfig;
  providers: Row[];
  sampleOf: (model: string) => Four;
  onClose: () => void;
  onPublish: (update: {versions: Row[]}, reason: string, check: string) => Promise<PublishOutcome>;
}) {
  const settings = readSettings(config.settings);
  const [mode, setMode] = useState<Mode>('multiplier');
  const [multiplierText, setMultiplierText] = useState(() => settings.defaultPrice !== null ? String(settings.defaultPrice) : '');
  const [percent, setPercent] = useState('');
  const [timing, setTiming] = useState<'soon' | 'custom'>('soon');
  const [customTime, setCustomTime] = useState(() => toLocalInput(soonSecs() + 3600));
  const [reason, setReason] = useState('');
  const [error, setError] = useState('');
  const [working, setWorking] = useState(false);
  const [preview, setPreview] = useState<PreviewPlan | null>(null);
  const nowSecs = adminApi.serverNowMs / 1000;
  const name = (model: Row) => modelName(model, config.models, config.groups);
  const typedMultiplier = typedNumber(multiplierText), typedPercent = /^[+\-−]?\d+(\.\d+)?$/.test(percent.trim()) ? Number(percent.trim().replace('−', '-')) : null;

  // One price per price table and model ID: entries sharing one are changed together.
  const entries: Entry[] = [];
  for (const mapping of models) {
    const rateCardId = String(config.groups.find(group => group.id === mapping.group_id)?.rate_card_id ?? ''), model = String(mapping.exposed_model_id);
    const key = `${rateCardId}\n${model}`, seen = entries.find(entry => entry.key === key);
    if (seen) {seen.mappings.push(mapping); continue;}
    const current = currentVersion(config.versions, rateCardId, [mapping.exposed_model_id, mapping.target_model], nowSecs), input = officialOf(current);
    const entry: Entry = {key, rateCardId, model, mappings: [mapping], current, legacy: !input};
    entries.push(entry);
    try {
      if (!rateCardId) throw new Error('分组没有价格表');
      const start = officialStart(settings, current, mapping);
      // Re-derived: the official price table's price for it (or its upstream) when there is one.
      const table = settings.official[model]?.usd ?? settings.official[String(mapping.target_model)]?.usd ?? null;
      let official = mode === 'rederive' ? table ?? start?.official : start?.official;
      if (!official) throw new Error('没有官方价：先在官方价表里补上，或单独调价');
      if (mode === 'scale') {
        if (typedPercent === null || typedPercent <= -100) throw new Error(typedPercent === null ? '填写百分比' : '降价不能达到或超过 100%');
        official = official.map(usd => Math.round(usd * (1 + typedPercent / 100) * 1e6) / 1e6) as Four;
      }
      const priceMultiplier = mode === 'multiplier' ? typedMultiplier : input?.priceMultiplier ?? settings.defaultPrice;
      if (priceMultiplier === null || !Number.isFinite(priceMultiplier)) throw new Error(mode === 'multiplier' ? '填写计费倍率' : '没有计费倍率：先在“定价设置”里设默认计费倍率');
      const cost = primaryCost(settings, mapping, official), costMultiplier = cost.costMultiplier ?? input?.costMultiplier ?? null;
      if (costMultiplier === null) throw new Error('主线路没有成本倍率：先在“定价设置”里给供应商设');
      if (settings.face === null) throw new Error('积分面值没有读到');
      const next = {official, priceMultiplier, costMultiplier, basis: cost.basis, usdCny: settings.usdCny, face: settings.face};
      const problem = officialProblem(next);
      if (problem) throw new Error(problem);
      entry.next = next;
    } catch (cause) {entry.problem = cause instanceof Error ? cause.message : String(cause);}
  }
  const ready = entries.filter(entry => entry.next && JSON.stringify(creditsOf(entry.next)) !== JSON.stringify(creditsOfVersion(entry.current))
    || entry.next && JSON.stringify(entry.next) !== JSON.stringify(officialOf(entry.current)));
  const skipped = entries.filter(entry => entry.problem);
  const typed = mode === 'multiplier' ? multiplierText.trim() : mode === 'scale' ? percent.trim() : 'ok';
  const blocked = working ? '正在发布' : !typed ? (mode === 'multiplier' ? '填写计费倍率' : '填写百分比') : !ready.length ? '没有能改的价格，见表格'
    : !validReason(reason) ? '填写原因后可预览（最多约 160 字）' : undefined;
  // All from one time: about a minute on (the first minute none of them has a price at) or the time chosen.
  const keys = (): Array<[unknown, unknown]> => ready.map(entry => [entry.rateCardId, entry.model]);
  const when = () => timing === 'soon' ? freeTime(config.versions, keys(), soonSecs()) : Math.floor(new Date(customTime).getTime() / 1000);
  const build = (at: number): Row[] => {
    const taken = config.versions.map(version => version.id);
    return ready.map(entry => {
      const priced = config.versions.some(version => version.rate_card_id === entry.rateCardId && version.model === entry.model), effective = priced ? at : 0;
      const id = versionIdFor(entry.model, effective || nowSecs, taken);
      taken.push(id);
      return officialVersion(entry.next!, {id, rateCardId: entry.rateCardId, model: entry.model, effectiveSecs: effective});
    });
  };
  // Every model the change touches, and each route's margin, at the sample requests.
  const impact = (versions: Row[], at: number) => {
    const now = adminApi.serverNowMs / 1000;
    return pricingImpact({settings, versions: config.versions}, {settings, versions: [...config.versions, ...versions.map(version => version.effective_from_secs === 0 ? {...version, effective_from_secs: now} : version)]},
      {models: config.models, groups: config.groups, nowSecs: now, effectiveSecs: at || now, sample: sampleOf});
  };
  const shownRows = (() => {try {return impact(build(soonSecs()), soonSecs());} catch {return [];}})();
  const rowOf = (entry: Entry) => shownRows.find(row => row.rateCardId === entry.rateCardId && row.model === entry.model);

  const open = () => {
    if (blocked) return;
    setError('');
    const at = when();
    if (!Number.isSafeInteger(at) || at <= adminApi.serverNowMs / 1000) {setError('生效时间需晚于现在'); return;}
    if (freeTime(config.versions, keys(), at) !== at) {setError('这一时刻已有其中某个模型的价格版本，请换一个生效时间'); return;}
    const versions = build(at), legacy = ready.filter(entry => entry.legacy);
    const others = ready.flatMap(entry => config.groups.filter(group => group.rate_card_id === entry.rateCardId && !entry.mappings.some(mapping => mapping.group_id === group.id)
      && config.models.some(model => model.group_id === group.id && model.exposed_model_id === entry.model)).map(group => `${String(group.name ?? group.id)} 的 ${entry.model}`));
    setPreview({title: `发布 ${versions.length} 个模型的新价格？`, rows: impact(versions, at), confirmLabel: '发布调价',
      facts: [mode === 'multiplier' ? `计费倍率都设为 ${timesText(typedMultiplier)}` : mode === 'scale' ? `官方价 ${typedPercent! > 0 ? '+' : ''}${typedPercent}%，计费倍率不变` : '按当前设置重算：官方价表、线路成本倍率、积分面值',
        ...(legacy.length ? [`这些模型原是旧版价格（直接填的积分），改为按官方价表定价：${nameList(legacy.map(entry => entry.model))}`] : []),
        ...(skipped.length ? [`不改：${nameList(skipped.map(entry => `${entry.model}（${entry.problem}）`), 4)}`] : []),
        ...(others.length ? [`同一价格表，同时生效于：${nameList(others)}`] : []),
        `生效：${timing === 'soon' ? '尽快，' : ''}${formatFullDateTime(at)} 起`, `原因：${reason.trim()}`],
      consequence: '到生效时间后，新请求按新价格扣费；已开始的请求按原价格结算。'});
  };
  const confirm = async () => {
    setPreview(null);
    const at = when();
    if (at <= adminApi.serverNowMs / 1000) {setError('生效时间已过，请重新选择'); return;}
    const versions = build(at);
    setWorking(true);
    const outcome = await onPublish({versions}, reason.trim(), `“价格版本”里这些模型有没有 ${formatFullDateTime(at)} 起的新价格：${nameList(ready.map(entry => entry.model))}`);
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
              {MODES.map(([value, label]) => <button key={value} type="button" role="radio" aria-checked={mode === value} onClick={() => setMode(value)}>{label}</button>)}
            </div></div>
          {mode === 'multiplier' && <label className="field"><span className="field-label">计费倍率<InfoTip text="我们的价格 = 官方价 × 计费倍率 × 官方价 $1 折合的人民币"/></span>
            <span className="input-suffix"><input aria-label="批量计费倍率" inputMode="decimal" placeholder="如 0.24" value={multiplierText} onChange={event => setMultiplierText(event.target.value)}/><span>×</span></span></label>}
          {mode === 'scale' && <label className="field"><span className="field-label">官方价变化<InfoTip text="正数涨、负数降，如 -10 表示官方价降 10%；计费倍率不变"/></span>
            <span className="input-suffix"><input aria-label="官方价变化百分比" inputMode="decimal" placeholder="如 -10" value={percent} onChange={event => setPercent(event.target.value)}/><span>%</span></span></label>}
          {mode === 'rederive' && <p className="muted field">按官方价表里的官方价、每条主线路现在的成本倍率和积分面值重新计算；计费倍率不变（旧版价格用默认计费倍率）。</p>}
        </div>
        <div className="table-scroll"><table className="table table-compact switch-preview bulk-preview" aria-label="批量调价预览">
          <thead><tr><th>模型 · 价格表</th><th>官方价 $ / 百万（入 / 出）</th><th className="num">计费倍率</th><th className="num">积分 / 百万（入 / 出）</th><th className="num">主线路毛利</th></tr></thead>
          <tbody>{entries.map(entry => {
            const before = creditsOfVersion(entry.current), after = entry.next ? creditsOf(entry.next) : null, input = officialOf(entry.current), row = rowOf(entry);
            const was = row?.before.routes[0]?.margin ?? null, will = row?.after.routes[0]?.margin ?? null;
            return <tr key={entry.key} className={entry.problem ? 'is-muted' : undefined}>
              <td><span className="mono">{name(entry.mappings[0])}</span>{entry.mappings.length > 1 && <span className="field-hint"> 同价：{entry.mappings.slice(1).map(mapping => String(config.groups.find(group => group.id === mapping.group_id)?.name ?? mapping.group_id)).join('、')}</span>}
                {entry.legacy && !entry.problem && <span className="field-hint">旧版价格：改按官方价表</span>}</td>
              <td>{entry.next ? `${usdText(entry.next.official[0])} / ${usdText(entry.next.official[1])}` : input ? `${usdText(input.official[0])} / ${usdText(input.official[1])}` : '—'}</td>
              <td className="num">{input ? timesText(input.priceMultiplier) : '—'}{entry.next && entry.next.priceMultiplier !== input?.priceMultiplier && <> → <b>{timesText(entry.next.priceMultiplier)}</b></>}</td>
              <td className="num">{entry.problem ? <span className="field-warning">{entry.problem}</span> : <>{before ? `${creditsText(before[0])} / ${creditsText(before[1])} → ` : ''}<b>{after ? `${creditsText(after[0])} / ${creditsText(after[1])}` : '—'}</b>
                {before && after && <span className="muted"> {percentChange(before[0], after[0])} / {percentChange(before[1], after[1])}</span>}</>}</td>
              <td className="num">{row ? <><span className={marginTone(was)}>{marginText(was)}</span> → <b className={marginTone(will)}>{marginText(will)}</b></> : ''}</td>
            </tr>;
          })}</tbody>
        </table></div>
        <p className="muted">毛利按每个模型近期请求的中位数（没有请求时 1,000 输入 + 1,000 输出）估算；预览里有每条线路的毛利。</p>
        <div className="field"><span className="field-label">生效时间</span>
          <div className="segmented" role="radiogroup" aria-label="生效时间">
            <button type="button" role="radio" aria-checked={timing === 'soon'} onClick={() => setTiming('soon')}>尽快（约 1 分钟后）</button>
            <button type="button" role="radio" aria-checked={timing === 'custom'} onClick={() => setTiming('custom')}>定时</button>
          </div>
          {timing === 'custom' && <input type="datetime-local" aria-label="定时生效时间" value={customTime} onChange={event => setCustomTime(event.target.value)}/>}
        </div>
        <label className="field"><span className="field-label">原因<span className="required-mark">（必填）</span></span>
          <input aria-label="批量调价原因" maxLength={500} placeholder="例：计费倍率统一调到 0.24" value={reason} onChange={event => setReason(event.target.value)}/></label>
      </fieldset>
      {error && <div role="alert" className="form-error"><p>{error}</p></div>}
    </div>
    <footer className="drawer-foot">
      <span className="muted">一次发布，每个价格表里的每个模型一个新价格版本</span>
      <span className="drawer-foot-spacer"/>
      <button type="button" className="btn" disabled={working} onClick={onClose}>取消</button>
      <button type="button" className="btn btn-primary" disabled={!!blocked} title={blocked} onClick={open}>{working ? '发布中…' : '预览调价'}</button>
    </footer>
    {preview && <PricingPreview plan={preview} providers={providers} groups={config.groups} onCancel={() => setPreview(null)} onConfirm={() => void confirm()}/>}
  </Drawer>;
}
