// 线路: the upstream targets that serve a model, and whether each can. A target (a provider and
// one of its upstream models) can serve when the provider is enabled and one of its enabled Keys
// may call that model and takes requests now: not refused as invalid (HTTP 401) and not cooling
// down (a Key tried again after its cooldown takes them), as the gateway picks Keys. Publishing
// checks a shown model's primary target; the backups take over, in order, when it cannot. Pure
// functions, so the rules can be tested without a browser.
import {formatClock, formatShortDate} from './format';
import {cooldownText, keyAlert} from './status';

type Row = Record<string, unknown>;

export interface Target {provider_id: string; target_model: string}
export interface TargetState extends Target {
  ok: boolean;
  problem?: 'no_provider' | 'provider_disabled' | 'no_key' | 'key_unhealthy' | 'key_cooldown';
  /** For a Key problem: the enabled Keys allowed the model, none of which takes requests now. */
  keys?: Row[];
  /** For a cooldown: when the first of them is back (seconds), when the server says. */
  until?: number;
}
export interface ModelRoute {primary: TargetState; backups: TargetState[]; /** Nothing can serve it. */ down: boolean}
/** `nowSecs` decides which cooldowns are over; the current time when left out. */
export interface RouteData {providers: Row[]; keys: Row[]; nowSecs?: number}

/** A Key permits this upstream model (an old Key without a list permits any). */
export const keyAllows = (key: Row, model: string) => !Array.isArray(key.allowed_models) || key.allowed_models.map(String).includes(model);

/** Whether an enabled Key of this provider may call this upstream model (its permission, whatever its health). */
export const canRoute = (providerId: unknown, model: string, keys: Row[]) =>
  keys.some(key => key.provider_id === providerId && key.enabled !== false && keyAllows(key, model));

/** A Key's live health lets it take requests now: not unhealthy, not cooling down (degraded does). */
export function keyServes(key: Row, nowSecs = Date.now() / 1000): boolean {
  const alert = keyAlert(key, nowSecs);
  return alert !== 'unhealthy' && alert !== 'cooldown';
}

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

export function targetState(target: Target, {providers, keys, nowSecs = Date.now() / 1000}: RouteData): TargetState {
  const provider = providers.find(item => item.id === target.provider_id);
  if (!provider) return {...target, ok: false, problem: 'no_provider'};
  if (provider.enabled === false) return {...target, ok: false, problem: 'provider_disabled'};
  const allowed = keys.filter(key => key.provider_id === target.provider_id && key.enabled !== false && keyAllows(key, target.target_model));
  if (!allowed.length) return {...target, ok: false, problem: 'no_key'};
  if (allowed.some(key => keyServes(key, nowSecs))) return {...target, ok: true};
  const cooling = allowed.filter(key => keyAlert(key, nowSecs) === 'cooldown');
  if (!cooling.length) return {...target, ok: false, problem: 'key_unhealthy', keys: allowed};
  const ends = cooling.map(key => Number(key.cooldown_until)).filter(until => until > nowSecs);
  return {...target, ok: false, problem: 'key_cooldown', keys: cooling, ...(ends.length ? {until: Math.min(...ends)} : {})};
}

export function modelRoute(model: Row, data: RouteData): ModelRoute {
  const [primary, ...backups] = targetsOf(model).map(target => targetState(target, data));
  return {primary, backups, down: !primary.ok && !backups.some(backup => backup.ok)};
}

/** A time the operator can find on their clock: 14:32 today, otherwise 09-28 14:32. */
function clockText(secs: number, nowSecs: number): string {
  return new Date(secs * 1000).toDateString() === new Date(nowSecs * 1000).toDateString() ? formatClock(secs) : `${formatShortDate(secs)} ${formatClock(secs)}`;
}

/** Why a target cannot serve, in a few words: 瀚月 Max 的 Key 返回 401. */
export function targetProblem(state: TargetState, providers: Row[], nowSecs = Date.now() / 1000): string {
  const name = String(providers.find(provider => provider.id === state.provider_id)?.name ?? state.provider_id);
  const keys = state.keys ?? [], many = keys.length > 1 ? `${keys.length} 个 Key ` : 'Key ';
  if (state.problem === 'no_provider') return `供应商 ${state.provider_id || '（空）'} 不存在`;
  if (state.problem === 'provider_disabled') return `${name} 已停用`;
  if (state.problem === 'no_key') return `${name} 没有启用的 Key 授权 ${state.target_model}`;
  if (state.problem === 'key_unhealthy') {
    const codes = [...new Set(keys.map(key => /^http_(\d{3})$/.exec(String(key.last_error ?? ''))?.[1] ?? ''))];
    const said = codes.length === 1 && codes[0] ? `返回 ${codes[0]}` : '不可用';
    return `${name} 的 ${many}${keys.length > 1 ? '都' : ''}${said}`;
  }
  if (state.problem === 'key_cooldown') {
    const back = typeof state.until === 'number' ? `，${keys.length > 1 ? '最早' : ''}${cooldownText(state.until - nowSecs)}后（${clockText(state.until, nowSecs)}）恢复` : '';
    return `${name} 的 ${many}${keys.length > 1 ? '都在冷却' : '冷却中'}${back}`;
  }
  return '';
}

/**
 * Whether a target will serve without anyone acting: it does now, or its Keys are only cooling
 * down. What a change takes away is judged by this, so the loss of a Key that is cooling down
 * counts, and a Key refused as invalid (it serves nothing until someone fixes it) does not.
 */
const willServe = (state: TargetState) => state.ok || state.problem === 'key_cooldown';

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

/** What a change to providers or Keys takes from live models (by the targets that will serve, see willServe). */
export function routeLosses(models: Row[], before: RouteData, after: RouteData): RouteLosses {
  const losses: RouteLosses = {down: [], takeover: [], backup: []};
  for (const model of models.filter(isLive)) {
    const was = modelRoute(model, before), now = modelRoute(model, after);
    if (!willServe(was.primary) && !was.backups.some(willServe)) continue;
    if (!willServe(now.primary) && !now.backups.some(willServe)) losses.down.push(model);
    else if (willServe(was.primary) && !willServe(now.primary)) losses.takeover.push(model);
    else if (was.backups.some((backup, index) => willServe(backup) && !willServe(now.backups[index]))) losses.backup.push(model);
  }
  return losses;
}

/**
 * Shown models the server will not let this Key's deletion strand: it is the last enabled Key
 * of their primary target's provider allowed to call that model, whatever its live health.
 */
export function deleteBlockers(models: Row[], keys: Row[], key: Row): Row[] {
  const serves = (item: Row, model: Row) => item.provider_id === model.target_provider_id && item.enabled !== false && keyAllows(item, String(model.target_model ?? ''));
  const others = keys.filter(item => !(item.id === key.id && item.provider_id === key.provider_id));
  return models.filter(model => isLive(model) && serves(key, model) && !others.some(item => serves(item, model)));
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
