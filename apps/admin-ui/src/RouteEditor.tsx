// 线路, below the model's primary route: the backups, tried in order when the route before them
// cannot serve (at most 8), each with its Key check, and what every route costs us. A route's own
// procurement price (线路采购价) is a version kept under `<provider>/<upstream model>` in the
// group's price table; it is staged in the draft here and published with the bar.
import {useState} from 'react';
import {Tag} from './components/ui';
import {buildRouteCost, COST_FIELDS, costText, routeCost, routeCostModel, type CostSource} from './priceChange';
import {authorizedModels, targetProblem, targetsOf, targetState, type Target} from './routes';

type Row = Record<string, unknown>;
export const MAX_BACKUPS = 8;
const SOURCE: Record<CostSource, string> = {route: '线路采购价', upstream: '按上游模型的价格版本', wildcard: '按价格表的 * 版本', model: '按这个模型的价格版本'};
const blankCosts = () => Object.fromEntries(COST_FIELDS.map(([field]) => [field, '']));
const domId = (text: string) => text.replace(/[^\w-]/g, '_');

/** A route's cost: staged, found as billing finds it, or not set; 设置采购价 stages a new one. */
function RouteCost({label, primary, target, model, rateCardId, versions, staged, nowSecs, taken, onStage}: {
  label: string; primary: boolean; target: Target; model: Row; rateCardId: string | null; versions: Row[]; staged: Row[]; nowSecs: number; taken: unknown[];
  onStage: (version: Row | null, costModel: string) => void;
}) {
  const [form, setForm] = useState<{costs: Record<string, string>; currency: string} | null>(null);
  const [error, setError] = useState('');
  if (!rateCardId || !target.provider_id || !target.target_model) return null;
  const costModel = routeCostModel(target.provider_id, target.target_model);
  const pending = staged.find(version => version.rate_card_id === rateCardId && version.model === costModel);
  const found = routeCost(versions, rateCardId, target, model, nowSecs);
  const open = () => {
    const base = pending ?? (found.source === 'route' ? found.version : null);
    setError(''); setForm({currency: ['USD', 'CNY'].includes(String(base?.currency)) ? String(base!.currency) : 'CNY',
      costs: base ? Object.fromEntries(COST_FIELDS.map(([field]) => [field, typeof base[field] === 'number' ? String(base[field]) : ''])) : blankCosts()});
  };
  const stage = () => {
    if (!form) return;
    try {onStage(buildRouteCost(form, {providerId: target.provider_id, targetModel: target.target_model, rateCardId, nowSecs, taken}), costModel); setForm(null);}
    catch (cause) {setError(cause instanceof Error ? cause.message : String(cause));}
  };
  return <div className="route-cost">
    {pending ? <span>采购 {costText(pending)} · <b>线路采购价，随发布生效</b> <button type="button" className="btn-text btn-small" onClick={() => onStage(null, costModel)}>撤销</button></span>
      : <span className={found.source === 'route' ? undefined : 'muted'} title={found.version ? `版本 ${String(found.version.id)}` : undefined}>
        {!found.version ? '采购价未设置' : costText(found.version) === '—' ? `采购价没填（${SOURCE[found.source!]}里没有采购价）` : `采购 ${costText(found.version)} · ${SOURCE[found.source!]}`}
        {/* A backup charged at another route's cost is the case a 线路采购价 is for. */}
        {!primary && found.version && found.source !== 'route' ? '（这条线路没有自己的采购价）' : ''}</span>}
    {!form && <button type="button" className="btn-text btn-small" onClick={open}>设置采购价</button>}
    {form && <div className="route-cost-form" role="group" aria-label={`${label}采购价`}>
      {COST_FIELDS.map(([field, name]) => <label key={field} className="field"><span className="field-label">{name}</span>
        <input aria-label={`${label}采购${name}价`} type="number" min="0" step="any" value={form.costs[field] ?? ''} onChange={event => setForm({...form, costs: {...form.costs, [field]: event.target.value}})}/></label>)}
      <label className="field"><span className="field-label">币种</span>
        <select aria-label={`${label}采购价币种`} value={form.currency} onChange={event => setForm({...form, currency: event.target.value})}><option value="CNY">CNY</option><option value="USD">USD</option></select></label>
      <span className="button-row"><button type="button" className="btn btn-small" onClick={stage}>暂存</button><button type="button" className="btn-text btn-small" onClick={() => setForm(null)}>取消</button></span>
      {error && <span className="field-error">{error}</span>}
    </div>}
  </div>;
}

export default function RouteEditor({model, rateCardId, versions, staged, providers, keys, nowSecs, onChain, onPromote, onStage}: {
  /** The model as the draft has it. */
  model: Row;
  rateCardId: string | null;
  /** Published versions, and the draft's new ones (a route cost staged here among them). */
  versions: Row[];
  staged: Row[];
  providers: Row[];
  keys: Row[];
  nowSecs: number;
  onChain: (backups: Target[]) => void;
  /** Makes this backup the primary route; the primary takes its place among the backups. */
  onPromote: (index: number) => void;
  onStage: (version: Row | null, costModel: string) => void;
}) {
  const [primary, ...backups] = targetsOf(model);
  const data = {providers, keys};
  const taken = [...versions, ...staged].map(version => version.id);
  const cost = (label: string, target: Target, primaryRoute = false) => <RouteCost key={`${label}:${target.provider_id}/${target.target_model}`} label={label} primary={primaryRoute} target={target} model={model}
    rateCardId={rateCardId} versions={versions} staged={staged} nowSecs={nowSecs} taken={taken} onStage={onStage}/>;
  const check = (target: Target) => {
    const state = targetState(target, data);
    return state.ok ? <Tag tone="success">可用</Tag> : <Tag tone="warning" title={targetProblem(state, providers)}>{targetProblem(state, providers)}</Tag>;
  };
  const set = (index: number, patch: Partial<Target>) => onChain(backups.map((backup, i) => i === index ? {...backup, ...patch} : backup));
  const move = (index: number, step: number) => {
    const next = [...backups]; [next[index], next[index + step]] = [next[index + step], next[index]]; onChain(next);
  };
  const add = () => onChain([...backups, {provider_id: String(providers.find(provider => provider.id !== primary.provider_id && provider.enabled !== false)?.id ?? primary.provider_id), target_model: primary.target_model}]);
  const listId = (index: number) => `backup-models-${domId(String(model.id ?? 'new'))}-${index}`;
  return <div className="route-list">
    <div className="route-primary">{check(primary)}{cost('主线路', primary, true)}</div>
    <ol className="route-backups" aria-label="备用线路">
      {backups.map((backup, index) => <li key={index} className="route-row">
        <span className="route-index">备 {index + 1}</span>
        <select aria-label={`备用线路 ${index + 1} 供应商`} value={backup.provider_id} onChange={event => set(index, {provider_id: event.target.value})}>
          {!providers.some(provider => provider.id === backup.provider_id) && <option value={backup.provider_id}>{backup.provider_id ? `${backup.provider_id}（未找到）` : '请选择'}</option>}
          {providers.map(provider => <option key={String(provider.id)} value={String(provider.id)}>{String(provider.name ?? provider.id)}{provider.enabled === false ? '（已停用）' : ''}</option>)}
        </select>
        <input aria-label={`备用线路 ${index + 1} 上游模型`} list={listId(index)} placeholder="选择或输入" value={backup.target_model} onChange={event => set(index, {target_model: event.target.value})}/>
        <datalist id={listId(index)}>{authorizedModels(backup.provider_id, keys).map(name => <option key={name} value={name}/>)}</datalist>
        {check(backup)}
        {backup.provider_id === primary.provider_id && backup.target_model === primary.target_model && <Tag tone="warning">和主线路相同</Tag>}
        <span className="route-actions">
          <button type="button" className="btn-icon" aria-label={`备用线路 ${index + 1} 上移`} title="上移" disabled={index === 0} onClick={() => move(index, -1)}>▲</button>
          <button type="button" className="btn-icon" aria-label={`备用线路 ${index + 1} 下移`} title="下移" disabled={index === backups.length - 1} onClick={() => move(index, 1)}>▼</button>
          <button type="button" className="btn-text btn-small" onClick={() => onPromote(index)}>设为主线路</button>
          <button type="button" className="btn-text btn-small is-danger" onClick={() => onChain(backups.filter((_, i) => i !== index))}>移除</button>
        </span>
        {cost(`备用线路 ${index + 1} `, backup)}
      </li>)}
    </ol>
    <button type="button" className="btn-text btn-small" disabled={backups.length >= MAX_BACKUPS} title={backups.length >= MAX_BACKUPS ? `最多 ${MAX_BACKUPS} 条备用线路` : '主线路不能用时，按顺序改走备用线路'} onClick={add}>＋ 添加备用线路</button>
  </div>;
}
