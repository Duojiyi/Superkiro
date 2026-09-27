// 定价设置: what every price is computed with — 积分面值, what an official $1 counts in ¥, the
// default 计费倍率 and 成本倍率, and each provider's 成本倍率 (with the routes that bill another
// way). Every change opens the preview of the models it touches and publishes in one
// revision-checked publication with a reason. A new face value (or ¥ per $1) reprices every
// official price in the same publication; a cost multiplier changes costs only, with keeping each
// margin offered separately. 美元汇率 stays, under 高级, for older cost-plus prices only.
import {useEffect, useState} from 'react';
import {pricingNow} from './clock';
import {adminApi, type AdminCardItem, type CommercialConfig} from './api';
import {confirmAction} from './components/confirm';
import {InfoTip} from './components/ui';
import {formatClock, formatCount, formatDateTime} from './format';
import {faceValuePlan, keepMarginPlan, multiplierOk, PLANS, pricingImpact, readSettings, type Four} from './officialPricing';
import PricingPreview, {type PreviewPlan} from './PricingPreview';
import {timesText, typedNumber, usdText, yuanText} from './pricingText';
import type {PublishOutcome} from './refusal';
import {nameList, targetsOf, type Target} from './routes';

type Row = Record<string, unknown>;
type Form = {face: string; usdCny: string; defaultPrice: string; defaultCost: string; legacyRate: string; providers: Record<string, string>};
export type Publish = (update: Record<string, unknown>, reason: string, action: string, done: string, check: string) => Promise<PublishOutcome>;

const text = (value: unknown) => typeof value === 'number' && Number.isFinite(value) ? String(value) : '';
const validReason = (value: string) => !!value.trim() && new TextEncoder().encode(value.trim()).length <= 500 && !/[\x00-\x1f\x7f-\x9f]/.test(value);
// Settings take effect for requests reserved after publishing; prices it adds start then too.
const serverNow = pricingNow;
const soon = () => Math.ceil((serverNow() + 60) / 60) * 60;

function formOf(settings: Row | undefined, providers: Row[]): Form {
  const current = (settings?.provider_cost_multipliers && typeof settings.provider_cost_multipliers === 'object' ? settings.provider_cost_multipliers : {}) as Row;
  const ids = [...new Set([...providers.map(provider => String(provider.id)), ...Object.keys(current)])];
  return {face: text(settings?.credit_face_value_cny), usdCny: text(settings?.official_usd_cny) || '1', defaultPrice: text(settings?.default_price_multiplier),
    defaultCost: text(settings?.default_cost_multiplier), legacyRate: text(settings?.usd_cny_rate), providers: Object.fromEntries(ids.map(id => [id, text(current[id])]))};
}

/** Plans as the server names them (`planPrices`), else the ones issued today. */
function plansOf(value: unknown): Array<{name: string; points: number; price: number}> {
  const rows = Array.isArray(value) ? value : value && typeof value === 'object' ? Object.entries(value as Row).map(([points, price]) => ({points: Number(points), price})) : [];
  const plans = rows.map(row => row as Row).map(row => ({name: String(row.name ?? `${formatCount(row.points)} 积分`), points: Number(row.points), price: Number(row.priceCny ?? row.price_cny ?? row.price)}))
    .filter(plan => plan.points > 0 && plan.price > 0);
  return plans.length ? plans : PLANS;
}

/**
 * The settings a publication sends: the two the server always needs, and only the newer ones
 * that changed (left out, they keep their value). Throws with the reason, in the words shown.
 */
export function settingsChange(form: Form, settings: Row): {payload: Row; changes: string[]} {
  const current = readSettings(settings), changes: string[] = [];
  const read = (value: string, label: string, check: (n: number) => boolean, rule: string, required = true) => {
    const number = typedNumber(value);
    if (number === null) {if (required) throw new Error(`请填写${label}`); return null;}
    if (!Number.isFinite(number) || !check(number)) throw new Error(`${label}${rule}`);
    return number;
  };
  const face = read(form.face, '积分面值', n => n >= 0.0001 && n <= 1000, '须在 0.0001–1000 元之间')!;
  const legacyRate = read(form.legacyRate, '旧版成本加成汇率', n => n > 0 && n <= 1000, '须大于 0、不超过 1000')!;
  const usdCny = read(form.usdCny, '官方价 $1 折合的人民币', n => n > 0 && n <= 1000, '须大于 0、不超过 1000')!;
  const defaultPrice = read(form.defaultPrice, '默认计费倍率', multiplierOk, '须大于 0、不超过 100', current.defaultPrice !== null);
  const defaultCost = read(form.defaultCost, '默认成本倍率', multiplierOk, '须大于 0、不超过 100', current.defaultCost !== null);
  const payload: Row = {credit_face_value_cny: face, usd_cny_rate: legacyRate};
  if (face !== current.face) changes.push(`积分面值 ${current.face ?? '—'} → ${face} 元/积分（1000 积分 = ¥${+(face * 1000).toFixed(4)}）`);
  if (usdCny !== current.usdCny) {payload.official_usd_cny = usdCny; changes.push(`官方价 $1 = ¥${current.usdCny} → ¥${usdCny}`);}
  if (defaultPrice !== null && defaultPrice !== current.defaultPrice) {payload.default_price_multiplier = defaultPrice; changes.push(`默认计费倍率 ${timesText(current.defaultPrice)} → ${timesText(defaultPrice)}（之后新定的价格用它）`);}
  if (defaultCost !== null && defaultCost !== current.defaultCost) {payload.default_cost_multiplier = defaultCost; changes.push(`默认成本倍率 ${timesText(current.defaultCost)} → ${timesText(defaultCost)}`);}
  const providers: Record<string, number> = {}, moved: string[] = [];
  for (const [id, value] of Object.entries(form.providers)) {
    const number = read(value, `${id} 的成本倍率`, multiplierOk, '须大于 0、不超过 100', false);
    if (number !== null) providers[id] = number;
    const was = current.providers[id] ?? null;
    if (number !== was) moved.push(`${id} ${was === null ? '默认' : timesText(was)} → ${number === null ? '默认' : timesText(number)}`);
  }
  if (moved.length) {payload.provider_cost_multipliers = providers; changes.push(`供应商成本倍率：${nameList(moved, 4)}`);}
  if (legacyRate !== current.legacyRate) changes.push(`旧版成本加成汇率 ${current.legacyRate ?? '—'} → ${legacyRate}（只影响用美元填采购价的旧版本）`);
  return {payload, changes};
}

export default function PricingSettings({config, readAt, providers, cards, sample, blocked, onPublish, onReload, onDirtyChange, onOpenOfficial, onEditRoute}: {
  config: CommercialConfig;
  /** When the configuration was read (server seconds). */
  readAt?: number;
  providers: Row[];
  cards?: AdminCardItem[];
  sample: (model: string) => Four;
  /** Why nothing can be published from here now (another change is unpublished). */
  blocked?: string;
  onPublish: Publish;
  /** Reads the configuration again after a conflict; what was typed here stays. */
  onReload: () => Promise<void>;
  onDirtyChange: (dirty: boolean) => void;
  /** Opens 官方价表 on this model name, to set its official price first. */
  onOpenOfficial: (name: string) => void;
  /** Opens a route's own cost (成本倍率 for this route, 上游计费基准). */
  onEditRoute: (target: Target) => void;
}) {
  const settings = (config.settings ?? {}) as unknown as Row;
  const [form, setForm] = useState<Form>(() => formOf(settings, providers));
  const [reason, setReason] = useState('');
  const [error, setError] = useState('');
  const [conflict, setConflict] = useState(false);
  const [preview, setPreview] = useState<{plan: PreviewPlan; publish: (option: boolean) => Promise<void>} | null>(null);
  const [working, setWorking] = useState(false);
  const pristine = formOf(settings, providers);
  // A provider left blank has no entry, whether or not it was listed when the form began.
  const same = (a: Form, b: Form) => JSON.stringify({...a, providers: Object.entries(a.providers).filter(([, value]) => value.trim()).sort()})
    === JSON.stringify({...b, providers: Object.entries(b.providers).filter(([, value]) => value.trim()).sort()});
  const dirty = !same(form, pristine) || !!reason.trim();
  const providerIds = [...new Set([...Object.keys(pristine.providers), ...Object.keys(form.providers)])];
  useEffect(() => {onDirtyChange(dirty);}, [dirty, onDirtyChange]);
  // Another publication moved the settings: a form nobody has touched follows them.
  useEffect(() => {if (!dirty) setForm(formOf(settings, providers));
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [config.revision, providers.length]);
  const current = readSettings(settings);
  const set = (patch: Partial<Form>) => {setForm(value => ({...value, ...patch})); setError('');};
  const face = typedNumber(form.face);
  const routesOf = (providerId: string) => config.models.flatMap(model => targetsOf(model).filter(target => target.provider_id === providerId).map(target => ({model, target})));
  const overrides = Object.entries(current.routes);
  const blockedReason = working ? '正在发布' : blocked ? blocked : !validReason(reason) ? '填写原因后可预览（最多约 160 字）' : undefined;

  const open = async () => {
    setError(''); setConflict(false);
    let change: ReturnType<typeof settingsChange>;
    try {change = settingsChange(form, settings);} catch (cause) {setError(cause instanceof Error ? cause.message : String(cause)); return;}
    if (!change.changes.length) {setError('设置没有变化'); return;}
    const now = serverNow(), before = readSettings(settings), after = readSettings({...settings, ...change.payload});
    const context = {models: config.models, groups: config.groups, nowSecs: now, effectiveSecs: now, sample};
    const repricing = after.face !== before.face || after.usdCny !== before.usdCny;
    const plan = repricing ? faceValuePlan(config.versions, {face: after.face!, usdCny: after.usdCny}, now) : null;
    const versionsAfter = plan ? [...config.versions.filter(version => !plan.cancelled.includes(String(version.id))), ...plan.versions.map(version => version.effective_from_secs === 0 ? {...version, effective_from_secs: now} : version)] : config.versions;
    const rows = pricingImpact({settings: before, versions: config.versions}, {settings: after, versions: versionsAfter}, context);
    const later = soon(), keep = repricing ? [] : keepMarginPlan(before, after, config, now, later);
    const keepRows = keep.length ? pricingImpact({settings: before, versions: config.versions}, {settings: after, versions: [...config.versions, ...keep.map(entry => entry.version)]}, {...context, effectiveSecs: later}) : [];
    let extra: PreviewPlan['extra'];
    if (plan) {
      // What unspent balances are worth, and each plan's ¥ per credit, at the new face value.
      const financials = await adminApi.getFinancials().catch(() => null) as (Record<string, unknown> | null);
      const liability = financials?.liability as Row | undefined;
      const usable = (cards ?? []).filter(card => ['active', 'unactivated'].includes(card.status) && card.archivedAt == null && !(card.validUntil && card.validUntil <= now));
      const balance = typeof liability?.credits === 'number' ? liability.credits : cards ? usable.reduce((sum, card) => sum + card.availableCredits, 0) : null;
      const legacy = [...new Set(plan.legacy.map(version => String(version.model)).filter(model => config.models.some(row => row.exposed_model_id === model || row.target_model === model)))];
      extra = <div className="preview-extra">
        {balance !== null && <p>未消耗余额 {formatCount(balance / 1_000_000)} 积分：折合 {yuanText(balance / 1_000_000 * before.face!)} → <b>{yuanText(balance / 1_000_000 * after.face!)}</b>{typeof liability?.credits === 'number' ? '' : '（按已加载的卡密估算）'}</p>}
        <table className="table table-compact" aria-label="套餐与新面值"><thead><tr><th>套餐</th><th className="num">积分</th><th className="num">售价</th><th className="num">每积分</th><th className="num">新面值 ¥{after.face}</th></tr></thead>
          <tbody>{plansOf(financials?.planPrices).map(item => {const per = item.price / item.points, gap = (per - after.face!) / after.face! * 100;
            return <tr key={item.points}><td>{item.name}</td><td className="num">{formatCount(item.points)}</td><td className="num">¥{item.price}</td><td className="num">¥{+per.toFixed(4)}</td>
              <td className="num">{Math.abs(gap) < 0.05 ? '持平' : gap > 0 ? `贵 ${gap.toFixed(1)}%` : `便宜 ${(-gap).toFixed(1)}%`}</td></tr>;})}</tbody></table>
        {legacy.length > 0 && <div className="note-warning"><p>这些模型没有官方价，积分不变，折合人民币按新面值变化：{nameList(legacy, 8)}。可以先设官方价，再改面值。</p>
          <div className="button-row">{legacy.slice(0, 6).map(model => <button key={model} type="button" className="btn btn-small" onClick={() => {setPreview(null); onOpenOfficial(model);}}>先设 {model} 的官方价</button>)}</div></div>}
        {plan.versions.length > 0 && <p className="muted">一起重算 {plan.versions.length} 个官方价版本{plan.cancelled.length ? `，其中 ${plan.cancelled.length} 个已排期的撤回后在原时间重新排期` : ''}；进行中的请求按原价格结算。</p>}
      </div>;
    }
    const facts = [...change.changes, `原因：${reason.trim()}`, `基于 ${formatClock(readAt ?? now)} 读取的配置`];
    setPreview({
      plan: {title: '发布定价设置？', facts, rows, creditsMove: repricing, extra,
        option: keep.length ? {label: `同时把计费倍率调到保持毛利（${nameList(keep.map(entry => `${entry.model} ${timesText(entry.from)} → ${timesText(entry.to)}`), 4)}，约 1 分钟后生效）`, rows: keepRows} : undefined,
        consequence: repricing ? '面值和官方价版本一起生效：同样的积分从现在起换算成新的人民币价格，已开始的请求按原价格结算。' : '发布后新请求按新设置计算成本；客户价格不变。',
        confirmLabel: '发布', empty: '没有模型的价格或毛利因此改变（只影响之后新定的价格）。'},
      publish: async option => {
        const versions = [...(plan?.versions ?? []), ...(option ? keep.map(entry => entry.version) : [])];
        const update = {settings: change.payload, ...(versions.length ? {versions} : {}), ...(plan?.cancelled.length ? {cancelled_versions: plan.cancelled} : {})};
        setWorking(true);
        const outcome = await onPublish(update, reason.trim(), '发布', '已发布定价设置', '“定价设置”里的数值是否已是新的');
        setWorking(false);
        if (outcome.ok) {setReason(''); return;}
        if (!outcome.uncertain) {setError(outcome.message); setConflict(!!outcome.conflict);}
      },
    });
  };

  const multiplierField = (label: string, key: 'defaultPrice' | 'defaultCost', tip: string, placeholder: string) =>
    <label className="field"><span className="field-label">{label}<InfoTip text={tip}/></span>
      <span className="input-suffix"><input aria-label={label} inputMode="decimal" placeholder={placeholder} value={form[key]} onChange={event => set({[key]: event.target.value})}/><span>×</span></span></label>;

  return <section className="panel pricing-settings" aria-label="定价设置">
    <div className="panel-head"><h3>定价设置</h3>{current.face !== null && <span className="muted">上次更新 {formatDateTime(Number(settings.rate_updated_at_secs))}</span>}</div>
    <fieldset disabled={working} className="form-grid form-grid-2">
      <label className="field"><span className="field-label">积分面值<InfoTip text="1 积分值多少元：积分价 = 我们的人民币价 ÷ 面值；改了面值，用官方价定的价格全部按新面值重算"/></span>
        <span className="input-suffix"><input aria-label="积分面值" inputMode="decimal" value={form.face} onChange={event => set({face: event.target.value})}/><span>元/积分</span></span>
        <span className="field-hint">{face !== null && Number.isFinite(face) && face > 0 ? `1000 积分 = ¥${+(face * 1000).toFixed(4)}` : '请输入正数'}</span></label>
      <label className="field"><span className="field-label">官方价 $1 = ¥<InfoTip text="官方价 1 美元按多少元算（不是汇率）；我们的价格 = 官方价 × 计费倍率 × 它"/></span>
        <span className="input-suffix"><input aria-label="官方价 1 美元折合" inputMode="decimal" value={form.usdCny} onChange={event => set({usdCny: event.target.value})}/><span>元</span></span></label>
      {multiplierField('默认计费倍率', 'defaultPrice', '新定价时默认的计费倍率：我们的价格 = 官方价 × 它；每个模型可以单独改', '如 0.24')}
      {multiplierField('默认成本倍率', 'defaultCost', '供应商没有单独设成本倍率时用它：成本 = 上游计费基准 × 它', '如 0.08')}
    </fieldset>
    <div className="table-scroll"><table className="table table-compact provider-costs" aria-label="供应商成本倍率">
      <thead><tr><th>供应商</th><th className="num">成本倍率</th><th>线路</th><th>线路单独设置</th></tr></thead>
      <tbody>{providerIds.map(id => {
        const provider = providers.find(item => item.id === id), used = routesOf(id), own = overrides.filter(([key]) => key.startsWith(`${id}/`));
        const value = form.providers[id] ?? '', resolved = typedNumber(value) ?? typedNumber(form.defaultCost);
        return <tr key={id}>
          <td>{String(provider?.name ?? id)}{!provider && <span className="muted">（供应商已不存在）</span>}</td>
          <td className="num"><span className="input-suffix"><input aria-label={`${String(provider?.name ?? id)} 成本倍率`} inputMode="decimal" placeholder={form.defaultCost ? `默认 ${form.defaultCost}` : '未设'} value={value}
            disabled={working} onChange={event => set({providers: {...form.providers, [id]: event.target.value}})}/><span>×</span></span>
            {resolved === null && used.length > 0 && <span className="field-warning">未设成本倍率：这些线路的成本按旧版采购价算</span>}</td>
          <td title={used.map(entry => `${String(entry.model.exposed_model_id)} → ${entry.target.target_model}`).join('\n')}>{used.length ? `${used.length} 条（${nameList([...new Set(used.map(entry => entry.target.target_model))], 3)}）` : <span className="muted">没有模型用它</span>}</td>
          <td>{own.map(([key, cost]) => <div key={key} className="route-override">
            <span className="mono">{key.slice(id.length + 1)}</span>{cost.costMultiplier !== null && <span>成本倍率 {timesText(cost.costMultiplier)}</span>}
            {cost.basis && <span>计费基准 {cost.basis.map(usd => usdText(usd)).join(' / ')}</span>}
            <button type="button" className="btn-text btn-small" disabled={!!blocked} title={blocked} onClick={() => onEditRoute({provider_id: id, target_model: key.slice(id.length + 1)})}>编辑</button></div>)}
            {used.length > 0 && <button type="button" className="btn-text btn-small" disabled={!!blocked} title={blocked}
              onClick={() => onEditRoute({provider_id: id, target_model: used.find(entry => !own.some(([key]) => key === `${id}/${entry.target.target_model}`))?.target.target_model ?? used[0].target.target_model})}>＋ 单独设置一条线路</button>}</td>
        </tr>;
      })}</tbody>
    </table></div>
    <details className="price-advanced">
      <summary>高级：旧版成本加成版本使用的汇率</summary>
      <label className="field"><span className="field-label">旧版成本加成版本使用的汇率<InfoTip text="只给用美元填采购价的旧版本换算成本；新的成本都按官方价 × 成本倍率算，不用它"/></span>
        <span className="input-suffix"><input aria-label="旧版成本加成版本使用的汇率" inputMode="decimal" value={form.legacyRate} disabled={working} onChange={event => set({legacyRate: event.target.value})}/><span>CNY/USD</span></span></label>
    </details>
    <div className="editor-actions">
      {error && <div role="alert" className="message message-error"><p>{error}</p>{conflict && <button type="button" className="btn btn-small" disabled={working} onClick={() => void onReload().then(() => {setError(''); setConflict(false);})}>重新加载</button>}</div>}
      <div className="button-row settings-actions">
        <input className="action-bar-reason" aria-label="定价设置变更原因" placeholder="变更原因（必填）" maxLength={500} value={reason} disabled={working} onChange={event => setReason(event.target.value)}/>
        {dirty && <button type="button" className="btn" disabled={working} onClick={async () => {
          if (!(await confirmAction({title: '放弃未发布的修改？', consequence: '定价设置回到服务器上的数值。', confirmLabel: '放弃修改'}))) return;
          setForm(pristine); setReason(''); setError(''); setConflict(false);
        }}>放弃修改</button>}
        <button type="button" className="btn btn-primary" disabled={!!blockedReason} title={blockedReason} onClick={() => void open()}>{working ? '发布中…' : '预览并发布'}</button>
      </div>
    </div>
    {preview && <PricingPreview plan={preview.plan} providers={providers} groups={config.groups} onCancel={() => setPreview(null)}
      onConfirm={option => {const run = preview.publish; setPreview(null); void run(option);}}/>}
  </section>;
}
