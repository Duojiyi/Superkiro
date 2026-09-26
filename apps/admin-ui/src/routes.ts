// 线路: the upstream targets that serve a model, and whether each can. A target (a provider and
// one of its upstream models) can serve when the provider is enabled and one of its enabled Keys
// may call that model. Publishing checks a shown model's primary target; the backups take over,
// in order, when it cannot. Pure functions, so the rules can be tested without a browser.

type Row = Record<string, unknown>;

export interface Target {provider_id: string; target_model: string}
export interface TargetState extends Target {ok: boolean; problem?: 'no_provider' | 'provider_disabled' | 'no_key'}
export interface ModelRoute {primary: TargetState; backups: TargetState[]; /** Nothing can serve it. */ down: boolean}
export interface RouteData {providers: Row[]; keys: Row[]}

/** A Key permits this upstream model (an old Key without a list permits any). */
export const keyAllows = (key: Row, model: string) => !Array.isArray(key.allowed_models) || key.allowed_models.map(String).includes(model);

/** Whether an enabled Key of this provider may call this upstream model. */
export const canRoute = (providerId: unknown, model: string, keys: Row[]) =>
  keys.some(key => key.provider_id === providerId && key.enabled !== false && keyAllows(key, model));

/** Upstream models an enabled Key of this provider is authorised for, sorted. */
export function authorizedModels(providerId: unknown, keys: Row[]): string[] {
  return [...new Set(keys.filter(key => key.provider_id === providerId && key.enabled !== false && Array.isArray(key.allowed_models))
    .flatMap(key => (key.allowed_models as unknown[]).map(String)))].sort();
}

/** The primary target, then the backups in the order they are tried. */
export function targetsOf(model: Row): Target[] {
  const backups = Array.isArray(model.fallback_chain) ? model.fallback_chain as unknown[] : [];
  return [{provider_id: String(model.target_provider_id ?? ''), target_model: String(model.target_model ?? '')},
    ...backups.filter((entry): entry is Row => !!entry && typeof entry === 'object')
      .map(entry => ({provider_id: String(entry.provider_id ?? ''), target_model: String(entry.target_model ?? '')}))];
}

export function targetState(target: Target, {providers, keys}: RouteData): TargetState {
  const provider = providers.find(item => item.id === target.provider_id);
  if (!provider) return {...target, ok: false, problem: 'no_provider'};
  if (provider.enabled === false) return {...target, ok: false, problem: 'provider_disabled'};
  return canRoute(target.provider_id, target.target_model, keys) ? {...target, ok: true} : {...target, ok: false, problem: 'no_key'};
}

export function modelRoute(model: Row, data: RouteData): ModelRoute {
  const [primary, ...backups] = targetsOf(model).map(target => targetState(target, data));
  return {primary, backups, down: !primary.ok && !backups.some(backup => backup.ok)};
}

/** Why a target cannot serve, in a few words. */
export function targetProblem(state: TargetState, providers: Row[]): string {
  const name = String(providers.find(provider => provider.id === state.provider_id)?.name ?? state.provider_id);
  if (state.problem === 'no_provider') return `供应商 ${state.provider_id || '（空）'} 不存在`;
  if (state.problem === 'provider_disabled') return `${name} 已停用`;
  return state.problem === 'no_key' ? `${name} 没有启用的 Key 授权 ${state.target_model}` : '';
}

/** Listed and served: shown to customers and not retired. */
export const isLive = (model: Row) => model.visible !== false && model.retired !== true;

/** Live models whose primary target cannot serve, each with its route. */
export function brokenRoutes(models: Row[], data: RouteData): Array<{model: Row; route: ModelRoute}> {
  return models.filter(isLive).map(model => ({model, route: modelRoute(model, data)})).filter(entry => !entry.route.primary.ok);
}

export interface RouteLosses {
  /** Served now, by nothing afterwards: their requests would fail. */
  down: Row[];
  /** Their primary target stops serving; a backup takes over. */
  takeover: Row[];
  /** Still served by their primary (or a backup), but one of their backups stops serving. */
  backup: Row[];
}

/** What a change to providers or Keys takes from live models. */
export function routeLosses(models: Row[], before: RouteData, after: RouteData): RouteLosses {
  const losses: RouteLosses = {down: [], takeover: [], backup: []};
  for (const model of models.filter(isLive)) {
    const was = modelRoute(model, before), now = modelRoute(model, after);
    if (was.down) continue;
    if (now.down) losses.down.push(model);
    else if (was.primary.ok && !now.primary.ok) losses.takeover.push(model);
    else if (was.backups.some((backup, index) => backup.ok && !now.backups[index].ok)) losses.backup.push(model);
  }
  return losses;
}

/**
 * What a provider or Key change takes from the models customers see, as confirmation facts; the
 * models left without any route are the ones 同时隐藏这些模型 hides.
 */
export function lossFacts(losses: RouteLosses, name: (model: Row) => string): string[] {
  const names = (rows: Row[]) => nameList(rows.map(name));
  return [
    ...(losses.down.length ? [`将无可用线路（客户请求会失败）：${names(losses.down)}`] : []),
    ...(losses.takeover.length ? [`主线路失效，改由备用线路服务：${names(losses.takeover)}`] : []),
    ...(losses.backup.length ? [`少一条备用线路（仍可服务）：${names(losses.backup)}`] : []),
  ];
}

/**
 * 切换线路: the model's route fields with `target` as its primary; the old primary becomes the
 * first backup when kept (so switching back is one step), and at most 8 backups remain.
 */
export function switchedRoute(model: Row, target: Target, keepOld: boolean): Row {
  const [old, ...backups] = targetsOf(model);
  const same = (a: Target, b: Target) => a.provider_id === b.provider_id && a.target_model === b.target_model;
  const rest = backups.filter(backup => !same(backup, target) && !same(backup, old));
  return {target_provider_id: target.provider_id, target_model: target.target_model, fallback_chain: (keepOld && !same(old, target) ? [old, ...rest] : rest).slice(0, 8)};
}

/** A model as lists name it: its ID, and its group when another group has the same ID. */
export function modelName(model: Row, models: Row[], groups: Row[]): string {
  const id = String(model.exposed_model_id ?? model.id);
  if (!models.some(other => other !== model && other.id !== model.id && other.exposed_model_id === model.exposed_model_id)) return id;
  return `${id}（${String(groups.find(group => group.id === model.group_id)?.name ?? model.group_id)}）`;
}

/** Up to `limit` names joined with 、, then how many there are in all. */
export function nameList(names: string[], limit = 6): string {
  return `${names.slice(0, limit).join('、')}${names.length > limit ? ` 等 ${names.length} 个` : ''}`;
}
