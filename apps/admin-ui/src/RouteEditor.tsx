// 线路, below the model's primary route: the backups, tried in order when the route before them
// cannot serve (at most 8), each with its Key check, and what every route costs us — from the
// official prices and the route's 成本倍率 when both are known, as billing costs it, else from the
// price versions as before. A route's own cost (设置线路成本) lives in the pricing settings and is
// published on its own.
import {Tag} from './components/ui';
import {costOfRoute, type PricingSettings, type RouteCostView} from './officialPricing';
import Probe from './Probe';
import {providerName, timesText, yuanText} from './pricingText';
import {authorizedModels, canRoute, targetProblem, targetsOf, targetState, type Target} from './routes';

type Row = Record<string, unknown>;
export const MAX_BACKUPS = 8;
const LEGACY: Record<string, string> = {route: '线路采购价版本', upstream: '上游模型的价格版本', wildcard: '价格表的 * 版本', model: '这个模型的价格版本'};
const domId = (text: string) => text.replace(/[^\w-]/g, '_');

/** A route's cost in one line: ¥ per million (input / output), and where it comes from. */
export function routeCostText(cost: RouteCostView, providers: Row[], target: Target): string {
  if (!cost.perM) return cost.basis ? `成本未知：${providerName(providers, target.provider_id)} 没有成本倍率` : `成本未知：${target.target_model} 没有官方价，也没有采购价`;
  const amount = `成本 ${yuanText(cost.perM[0])} / ${yuanText(cost.perM[1])} 每百万（入 / 出）`;
  if (cost.how === 'official') return `${amount} · ${cost.basis?.source === 'route' ? '线路计费基准' : `${target.target_model} 官方价`} × ${timesText(cost.multiplier?.value)}（${cost.multiplier?.source === 'route' ? '这条线路' : cost.multiplier?.source === 'provider' ? '供应商' : '默认'}）`;
  return `${amount} · 旧版：按${LEGACY[cost.legacySource ?? ''] ?? '价格版本'}${cost.basis ? '' : `（${target.target_model} 还没有官方价）`}`;
}

export default function RouteEditor({model, rateCardId, versions, settings, providers, keys, nowSecs, onChain, onPromote, onEditRoute, editBlocked}: {
  /** The model as the draft has it. */
  model: Row;
  rateCardId: string | null;
  versions: Row[];
  settings: PricingSettings;
  providers: Row[];
  keys: Row[];
  nowSecs: number;
  onChain: (backups: Target[]) => void;
  /** Makes this backup the primary route; the primary takes its place among the backups. */
  onPromote: (index: number) => void;
  /** 设置线路成本: a route's own 成本倍率 and 上游计费基准. */
  onEditRoute: (target: Target) => void;
  /** Why a route's cost cannot be changed now (it publishes on its own). */
  editBlocked?: string;
}) {
  const [primary, ...backups] = targetsOf(model);
  const data = {providers, keys};
  const cost = (target: Target) => {
    if (!target.provider_id || !target.target_model) return null;
    const view = costOfRoute(settings, versions, rateCardId, target, model, nowSecs);
    return <div className="route-cost">
      <span className={view.how === 'official' ? undefined : 'muted'} title={view.version ? `版本 ${String(view.version.id)}` : undefined}>{routeCostText(view, providers, target)}</span>
      <button type="button" className="btn-text btn-small" disabled={!!editBlocked} title={editBlocked ?? '这条线路收费和供应商其他线路不同时，单独设成本倍率或计费基准'} onClick={() => onEditRoute(target)}>设置线路成本</button>
    </div>;
  };
  const check = (target: Target) => {
    const state = targetState(target, data);
    return <>{state.ok ? <Tag tone="success">可用</Tag> : <Tag tone="warning" title={targetProblem(state, providers)}>{targetProblem(state, providers)}</Tag>}
      <Probe providerId={target.provider_id} model={target.target_model} disabled={!state.ok}/></>;
  };
  const set = (index: number, patch: Partial<Target>) => onChain(backups.map((backup, i) => i === index ? {...backup, ...patch} : backup));
  const move = (index: number, step: number) => {
    const next = [...backups]; [next[index], next[index + step]] = [next[index + step], next[index]]; onChain(next);
  };
  // A new backup starts on another enabled provider that can serve this upstream model, if there is one.
  const add = () => {
    const others = providers.filter(provider => provider.id !== primary.provider_id && provider.enabled !== false);
    const serving = others.find(provider => canRoute(provider.id, primary.target_model, keys) && !backups.some(backup => backup.provider_id === provider.id && backup.target_model === primary.target_model));
    onChain([...backups, {provider_id: String((serving ?? others[0])?.id ?? primary.provider_id), target_model: primary.target_model}]);
  };
  const listId = (index: number) => `backup-models-${domId(String(model.id ?? 'new'))}-${index}`;
  return <div className="route-list">
    <div className="route-primary">{check(primary)}{cost(primary)}</div>
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
        {cost(backup)}
      </li>)}
    </ol>
    <button type="button" className="btn-text btn-small" disabled={backups.length >= MAX_BACKUPS} title={backups.length >= MAX_BACKUPS ? `最多 ${MAX_BACKUPS} 条备用线路` : '主线路不能用时，按顺序改走备用线路'} onClick={add}>＋ 添加备用线路</button>
  </div>;
}
