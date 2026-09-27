// 线路成本: what one route (a provider and one of its upstream models) really bills, when that is not
// its provider's 成本倍率 on the upstream model's official price — a 成本倍率 of its own, and/or
// another basis (e.g. hanyue-max bills claude-opus-5-5 at $2 / $25 / $6.25 / $0.5). Kept in the
// pricing settings, so it holds in every group and price table; published with a reason after the
// preview of the margins it moves.
import {useState} from 'react';
import {adminApi, type CommercialConfig} from './api';
import {IconClose} from './components/icons';
import {Drawer} from './components/modal';
import {InfoTip} from './components/ui';
import {formatDateTime} from './format';
import {costFor, KINDS, multiplierOk, pricingImpact, readSettings, routeKey, routeMultiplier, usdOk, type Four} from './officialPricing';
import PricingPreview, {type PreviewPlan} from './PricingPreview';
import type {Publish} from './PricingSettings';
import {providerName, timesText, typedNumber, usdText, yuanText} from './pricingText';
import type {Target} from './routes';

type Row = Record<string, unknown>;
const validReason = (value: string) => !!value.trim() && new TextEncoder().encode(value.trim()).length <= 500 && !/[\x00-\x1f\x7f-\x9f]/.test(value);

export default function RouteCostDrawer({target, config, providers, sample, blocked, onPublish, onClose}: {
  target: Target;
  config: CommercialConfig;
  providers: Row[];
  sample: (model: string) => Four;
  blocked?: string;
  onPublish: Publish;
  onClose: () => void;
}) {
  const raw = (config.settings ?? {}) as unknown as Row, settings = readSettings(raw);
  const key = routeKey(target.provider_id, target.target_model), current = settings.routes[key];
  const [multiplier, setMultiplier] = useState(current?.costMultiplier != null ? String(current.costMultiplier) : '');
  const [basis, setBasis] = useState<string[]>(current?.basis ? current.basis.map(String) : ['', '', '', '']);
  const [reason, setReason] = useState('');
  const [error, setError] = useState('');
  const [working, setWorking] = useState(false);
  const [preview, setPreview] = useState<{plan: PreviewPlan; publish: () => Promise<void>} | null>(null);
  const name = `${providerName(providers, target.provider_id)} / ${target.target_model}`;
  const inherited = routeMultiplier({...settings, routes: {}}, target), official = settings.official[target.target_model];
  // What the route would cost as typed (blank fields fall back as billing does).
  const typedMultiplier = typedNumber(multiplier), typedBasis = basis.map(typedNumber);
  const usedMultiplier = typedMultiplier ?? inherited?.value ?? null;
  const usedBasis = typedBasis.every(value => value === null) ? official?.usd ?? null : typedBasis.every(value => value !== null) ? typedBasis as Four : null;
  const perM = usedMultiplier !== null && usedBasis && Number.isFinite(usedMultiplier) && usedBasis.every(Number.isFinite) ? usedBasis.map(usd => costFor(usd, usedMultiplier, settings.usdCny)) : null;
  const blockedReason = working ? '正在发布' : blocked ? blocked : !validReason(reason) ? '填写原因后可预览（最多约 160 字）' : undefined;

  const open = (remove: boolean) => {
    setError('');
    const next: Record<string, Row> = {};
    for (const [route, cost] of Object.entries(settings.routes)) if (route !== key) next[route] = {...(cost.costMultiplier !== null ? {cost_multiplier: cost.costMultiplier} : {}), ...(cost.basis ? {basis_usd_per_m: cost.basis} : {})};
    const facts: string[] = [];
    if (!remove) {
      if (typedMultiplier !== null && (!Number.isFinite(typedMultiplier) || !multiplierOk(typedMultiplier))) {setError('成本倍率需大于 0、不超过 100（留空按供应商的）'); return;}
      if (typedBasis.some(value => value !== null) && !typedBasis.every(value => value !== null && Number.isFinite(value) && usdOk(value))) {setError('上游计费基准要四项都填（0–10,000 美元），或都留空按官方价'); return;}
      const entry: Row = {...(typedMultiplier !== null ? {cost_multiplier: typedMultiplier} : {}), ...(typedBasis.every(value => value !== null) ? {basis_usd_per_m: typedBasis} : {})};
      if (!Object.keys(entry).length) {setError('两项都留空时这条线路按供应商设置计算：点“删除这条设置”'); return;}
      next[key] = entry;
      facts.push(`${name}：成本倍率 ${timesText(current?.costMultiplier ?? null)} → ${typedMultiplier === null ? `按供应商 ${timesText(inherited?.value ?? null)}` : timesText(typedMultiplier)}`,
        `上游计费基准 ${current?.basis ? current.basis.map(usd => usdText(usd)).join(' / ') : '官方价'} → ${typedBasis.every(value => value !== null) ? (typedBasis as number[]).map(usd => usdText(usd)).join(' / ') : '官方价'}`);
    } else facts.push(`删除 ${name} 的单独设置：按供应商成本倍率 ${timesText(inherited?.value ?? null)} 和官方价计算`);
    const now = Math.floor(adminApi.serverNowMs / 1000), after = readSettings({...raw, route_costs: next});
    const rows = pricingImpact({settings, versions: config.versions}, {settings: after, versions: config.versions}, {models: config.models, groups: config.groups, nowSecs: now, effectiveSecs: now, sample});
    setPreview({
      plan: {title: remove ? `删除 ${name} 的线路成本设置？` : `发布 ${name} 的线路成本？`, facts: [...facts, `原因：${reason.trim()}`, `基于 ${formatDateTime(now)} 读取的配置`], rows,
        consequence: '只改成本：发布后，这条线路服务的请求按新成本计算，所有分组都一样；客户价格不变。', confirmLabel: '发布',
        empty: official || typedBasis.every(value => value !== null) ? '没有模型的毛利因此改变。' : `${target.target_model} 还没有官方价，也没填计费基准：这条线路的成本仍按旧版采购价算。`},
      publish: async () => {
        setWorking(true);
        const outcome = await onPublish({settings: {credit_face_value_cny: raw.credit_face_value_cny, usd_cny_rate: raw.usd_cny_rate, route_costs: next}}, reason.trim(), '发布', `已发布 ${name} 的线路成本`, `“定价设置”里 ${name} 的成本是否已是新的`);
        setWorking(false);
        if (outcome.ok) {onClose(); return;}
        if (!outcome.uncertain) setError(outcome.message); else onClose();
      },
    });
  };

  const copyFrom = (source: string) => {const price = settings.official[source]; if (price) setBasis(price.usd.map(String));};
  return <Drawer id="route-cost" label={`线路成本 · ${name}`} onClose={() => {if (!working) onClose();}} className="price-drawer route-cost-drawer">
    <header className="drawer-head">
      <div className="drawer-title"><span className="drawer-model">线路成本 · <span className="mono">{name}</span></span></div>
      <div className="drawer-tools"><button type="button" className="btn-icon" aria-label="关闭线路成本" title="关闭（Esc）" disabled={working} onClick={onClose}><IconClose/></button></div>
    </header>
    <div className="drawer-body">
      <fieldset disabled={working} className="price-form">
        <p className="muted">这条线路的成本 = 上游计费基准 × 成本倍率 × {settings.usdCny === 1 ? '¥1/$1' : `¥${settings.usdCny}/$1`}。留空的项按供应商设置和 {target.target_model} 的官方价算；在所有分组、所有价格表里都一样。</p>
        <label className="field"><span className="field-label">成本倍率（这条线路）<InfoTip text="只在这条线路收费和供应商其他线路不同时填"/></span>
          <span className="input-suffix"><input aria-label="这条线路的成本倍率" inputMode="decimal" placeholder={inherited ? `按供应商 ${inherited.value}` : '供应商和默认都没设'} value={multiplier} onChange={event => setMultiplier(event.target.value)}/><span>×</span></span></label>
        <div className="field"><span className="field-label">上游计费基准 · 美元 / 百万 Tokens<InfoTip text="上游按别的价格收费时填（例：瀚月 Max 按 Opus 5 的价格收 claude-opus-5-5）；留空按官方价"/></span>
          <div className="listing-grid">{KINDS.map((kind, index) => <label key={kind} className="field"><span className="field-label">{kind}</span>
            <input aria-label={`计费基准${kind}价`} inputMode="decimal" placeholder={official ? String(official.usd[index]) : '官方价'} value={basis[index]} onChange={event => setBasis(values => values.map((value, i) => i === index ? event.target.value : value))}/></label>)}</div>
          <label className="field"><span className="field-label">复制其他模型的官方价</span>
            <select aria-label="复制其他模型的官方价作为计费基准" value="" onChange={event => copyFrom(event.target.value)}>
              <option value="">选择模型…</option>
              {Object.keys(settings.official).sort().map(model => <option key={model} value={model}>{model}（{settings.official[model].usd.map(usd => usdText(usd)).join(' / ')}）</option>)}
            </select></label>
        </div>
        <p role="status" className="pricing-result">{perM ? <>成本 · ¥ / 百万：{KINDS.map((kind, index) => `${kind} ${yuanText(perM[index])}`).join(' · ')}</>
          : usedBasis ? '成本倍率还没有：在“定价设置”里给供应商或默认设一个' : `${target.target_model} 还没有官方价：先在“官方价表”里加上，或在这里填计费基准`}</p>
        <label className="field"><span className="field-label">原因<span className="required-mark">（必填）</span></span>
          <input aria-label="线路成本原因" maxLength={500} placeholder="例：上游按 Opus 5 的价格收费" value={reason} onChange={event => setReason(event.target.value)}/></label>
      </fieldset>
      {error && <div role="alert" className="form-error"><p>{error}</p></div>}
    </div>
    <footer className="drawer-foot">
      {current && <button type="button" className="btn-text is-danger" disabled={!!blockedReason} title={blockedReason} onClick={() => open(true)}>删除这条设置</button>}
      <span className="drawer-foot-spacer"/>
      <button type="button" className="btn" disabled={working} onClick={onClose}>取消</button>
      <button type="button" className="btn btn-primary" disabled={!!blockedReason} title={blockedReason} onClick={() => open(false)}>{working ? '发布中…' : '预览并发布'}</button>
    </footer>
    {preview && <PricingPreview plan={preview.plan} providers={providers} groups={config.groups} onCancel={() => setPreview(null)}
      onConfirm={() => {const run = preview.publish; setPreview(null); void run();}}/>}
  </Drawer>;
}
