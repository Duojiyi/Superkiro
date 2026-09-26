// 切换线路: move the chosen models to another provider in one publication. Each keeps its
// upstream model name unless changed here; the preview shows the route before and after, the
// Key check and what the new route costs us (a 线路采购价 can be set for it on the way). The
// confirmation offers to keep the old route as the first backup, so 切回 is one step.
import {useState} from 'react';
import {adminApi, type CommercialConfig} from './api';
import {ask} from './components/confirm';
import {IconClose} from './components/icons';
import {Drawer} from './components/modal';
import {toast} from './components/toast';
import {Tag} from './components/ui';
import {buildRouteCost, COST_FIELDS, costText, routeCost, timeDraftVersions, type CostSource} from './priceChange';
import type {PublishOutcome} from './refusal';
import {authorizedModels, isLive, modelName, nameList, switchedRoute, targetProblem, targetsOf, targetState, type Target} from './routes';

type Row = Record<string, unknown>;
/** One model's route before and after a switch: what 切回 restores. */
export interface SwitchedRoute {id: string; before: Row; after: Row}

const SOURCE: Record<CostSource, string> = {route: '线路采购价', upstream: '上游模型的价格版本', wildcard: '价格表的 *', model: '模型的价格版本'};
const validReason = (text: string) => !!text.trim() && new TextEncoder().encode(text.trim()).length <= 500 && !/[\x00-\x1f\x7f-\x9f]/.test(text);
const sameTarget = (a: Target, b: Target) => a.provider_id === b.provider_id && a.target_model === b.target_model;

export default function RouteSwitchDrawer({models, config, providers, keys, onClose, onPublish, onSwitched}: {
  /** The chosen models, as published. */
  models: Row[];
  config: CommercialConfig;
  providers: Row[];
  keys: Row[];
  onClose: () => void;
  onPublish: (update: {models: Row[]; versions?: Row[]}, reason: string, check: string) => Promise<PublishOutcome>;
  onSwitched: (provider: string, kept: boolean, routes: SwitchedRoute[]) => void;
}) {
  const [providerId, setProviderId] = useState(() => String(providers.find(provider => provider.enabled !== false && models.some(model => model.target_provider_id !== provider.id))?.id ?? providers[0]?.id ?? ''));
  const [targets, setTargets] = useState<Record<string, string>>(() => Object.fromEntries(models.map(model => [String(model.id), String(model.target_model ?? '')])));
  const [costs, setCosts] = useState<Record<string, {costs: Record<string, string>; currency: string}>>({});
  const [reason, setReason] = useState('');
  const [error, setError] = useState('');
  const [working, setWorking] = useState(false);
  const provider = providers.find(item => item.id === providerId);
  const providerName = String(provider?.name ?? providerId);
  const nowSecs = adminApi.serverNowMs / 1000;
  const name = (model: Row) => modelName(model, config.models, config.groups);
  const rows = models.map(model => {
    const id = String(model.id), target = {provider_id: providerId, target_model: (targets[id] ?? '').trim()};
    const rateCardId = config.groups.find(group => group.id === model.group_id)?.rate_card_id;
    const old = {provider_id: String(model.target_provider_id ?? ''), target_model: String(model.target_model ?? '')};
    return {model, id, target, old, rateCardId: typeof rateCardId === 'string' ? rateCardId : null, state: targetState(target, {providers, keys}),
      unchanged: sameTarget(old, target), oldCost: routeCost(config.versions, rateCardId, old, model, nowSecs),
      newCost: routeCost(config.versions, rateCardId, target, {...model, target_provider_id: target.provider_id, target_model: target.target_model}, nowSecs)};
  });
  const changing = rows.filter(row => !row.unchanged);
  const blocked = working ? '正在切换' : !changing.length ? '选中的模型已经在用这条线路' : !validReason(reason) ? '填写原因后可切换（最多约 160 字）' : undefined;

  const submit = async () => {
    if (blocked) return;
    setError('');
    const empty = changing.filter(row => !row.target.target_model);
    if (empty.length) {setError(`请填写新线路的上游模型：${nameList(empty.map(row => name(row.model)))}`); return;}
    // A shown model's primary route must be able to serve: the server refuses it otherwise.
    const stranded = changing.filter(row => isLive(row.model) && !row.state.ok);
    if (stranded.length) {setError(`这些在售模型的新线路不能用：${nameList(stranded.map(row => `${name(row.model)}（${targetProblem(row.state, providers)}）`), 4)}。先在“供应商与 Key”里处理，或先把它们隐藏`); return;}
    let versions: Row[];
    try {
      const taken = config.versions.map(version => version.id);
      versions = changing.filter(row => costs[row.id] && row.rateCardId).map(row => {
        const version = buildRouteCost(costs[row.id], {providerId, targetModel: row.target.target_model, rateCardId: row.rateCardId!, nowSecs, taken});
        taken.push(version.id);
        return version;
      });
    } catch (cause) {setError(cause instanceof Error ? cause.message : String(cause)); return;}
    const answer = await ask({
      title: `把 ${changing.length} 个模型切换到 ${providerName}？`,
      facts: [...changing.slice(0, 8).map(row => `${name(row.model)}：${row.old.provider_id} / ${row.old.target_model} → ${providerId} / ${row.target.target_model}`),
        ...(changing.length > 8 ? [`等 ${changing.length} 个`] : []),
        ...(versions.length ? [`同时设置 ${versions.length} 条新线路的采购价`] : []),
        `原因：${reason.trim()}`],
      option: {label: '保留原线路作为备用（新线路不能用时自动改走原线路，也方便切回）', checked: true},
      consequence: '发布后新请求立即走新线路，进行中的请求不受影响。',
      confirmLabel: '切换',
    });
    if (!answer.confirmed) return;
    const routes: SwitchedRoute[] = changing.map(row => ({id: row.id, before: {target_provider_id: row.model.target_provider_id, target_model: row.model.target_model, fallback_chain: targetsOf(row.model).slice(1)},
      after: switchedRoute(row.model, row.target, answer.option)}));
    setWorking(true);
    const later = Math.ceil((adminApi.serverNowMs / 1000 + 120) / 60) * 60;
    const outcome = await onPublish({models: changing.map((row, index) => ({...row.model, ...routes[index].after})), ...(versions.length ? {versions: timeDraftVersions(versions, config.versions, later)} : {})},
      reason.trim(), `这些模型的线路是否已是 ${providerName}：${nameList(changing.map(row => name(row.model)))}`);
    setWorking(false);
    if (outcome.ok) {toast.success(`已把 ${changing.length} 个模型切换到 ${providerName}`); onSwitched(providerId, answer.option, routes); onClose(); return;}
    if (outcome.uncertain) {onClose(); return;}
    setError(outcome.message);
  };

  return <Drawer id="route-switch" label="切换线路" onClose={() => {if (!working) onClose();}} className="price-drawer switch-drawer">
    <header className="drawer-head">
      <div className="drawer-title"><span className="drawer-model">切换线路</span><span className="muted">{models.length} 个模型</span></div>
      <div className="drawer-tools"><button type="button" className="btn-icon" aria-label="关闭切换线路" title="关闭（Esc）" disabled={working} onClick={onClose}><IconClose/></button></div>
    </header>
    <div className="drawer-body">
      <fieldset disabled={working} className="price-form">
        <label className="field"><span className="field-label">切换到供应商</span>
          <select aria-label="新供应商" value={providerId} onChange={event => setProviderId(event.target.value)}>
            {providers.map(item => <option key={String(item.id)} value={String(item.id)}>{String(item.name ?? item.id)}{item.enabled === false ? '（已停用）' : ''}</option>)}
          </select></label>
        <div className="table-scroll"><table className="table table-compact switch-preview">
          <thead><tr><th>模型</th><th>现在</th><th>切换后上游模型</th><th>Key 检查</th><th>采购价（现在 → 切换后）</th></tr></thead>
          <tbody>{rows.map(row => {
            const form = costs[row.id];
            return <tr key={row.id} className={row.unchanged ? 'is-muted' : undefined}>
              <td className="mono">{name(row.model)}</td>
              <td className="mono">{row.old.provider_id} / {row.old.target_model}</td>
              <td><input aria-label={`${name(row.model)} 切换后上游模型`} list={`switch-models-${row.id.replace(/[^\w-]/g, '_')}`} value={targets[row.id] ?? ''} onChange={event => setTargets({...targets, [row.id]: event.target.value})}/>
                <datalist id={`switch-models-${row.id.replace(/[^\w-]/g, '_')}`}>{authorizedModels(providerId, keys).map(model => <option key={model} value={model}/>)}</datalist></td>
              <td>{row.unchanged ? <span className="muted">已是这条线路</span> : row.state.ok ? <Tag tone="success">可用</Tag> : <Tag tone={isLive(row.model) ? 'danger' : 'warning'} title={targetProblem(row.state, providers)}>{targetProblem(row.state, providers)}</Tag>}</td>
              <td><div className="switch-cost">
                <span>{costText(row.oldCost.version)} → {form ? <b>新设</b> : row.newCost.version ? `${costText(row.newCost.version)}` : '未设置'}</span>
                {!form && row.newCost.source !== 'route' && !row.unchanged && <span className="field-warning">新线路没有自己的采购价{row.newCost.version ? `，成本会按${SOURCE[row.newCost.source!]}估算` : ''}</span>}
                {!row.unchanged && (form
                  ? <span className="switch-cost-form">{COST_FIELDS.map(([field, label]) => <input key={field} aria-label={`${name(row.model)} 新线路采购${label}价`} placeholder={label} type="number" min="0" step="any"
                      value={form.costs[field] ?? ''} onChange={event => setCosts({...costs, [row.id]: {...form, costs: {...form.costs, [field]: event.target.value}}})}/>)}
                    <select aria-label={`${name(row.model)} 新线路采购价币种`} value={form.currency} onChange={event => setCosts({...costs, [row.id]: {...form, currency: event.target.value}})}><option value="CNY">CNY</option><option value="USD">USD</option></select>
                    <button type="button" className="btn-text btn-small" onClick={() => {const next = {...costs}; delete next[row.id]; setCosts(next);}}>不设置</button></span>
                  : <button type="button" className="btn-text btn-small" onClick={() => setCosts({...costs, [row.id]: {currency: 'CNY', costs: Object.fromEntries(COST_FIELDS.map(([field]) => [field, '']))}})}>设置新线路采购价</button>)}
              </div></td>
            </tr>;
          })}</tbody>
        </table></div>
        <label className="field"><span className="field-label">原因<span className="required-mark">（必填）</span></span>
          <input aria-label="切换原因" maxLength={500} placeholder="例：主供应商故障，先切到备用供应商" value={reason} onChange={event => setReason(event.target.value)}/></label>
      </fieldset>
      {error && <div role="alert" className="form-error"><p>{error}</p></div>}
    </div>
    <footer className="drawer-foot">
      <span className="muted">上游模型名默认不变；确认时可以保留原线路作为备用</span>
      <span className="drawer-foot-spacer"/>
      <button type="button" className="btn" disabled={working} onClick={onClose}>取消</button>
      <button type="button" className="btn btn-primary" disabled={!!blocked} title={blocked} onClick={() => void submit()}>{working ? '切换中…' : '切换'}</button>
    </footer>
  </Drawer>;
}
