import {targetsOf} from './routes';
import type {Row} from './types';

export type RoutingMode = 'off' | 'observe' | 'enforce';
export interface RoutingClassifier {
  provider_id: string; model: string; timeout_ms: number; max_input_chars: number;
  daily_request_limit: number; daily_budget_micro_cny: number;
  input_price_micro_cny_per_million: number; output_price_micro_cny_per_million: number;
}
export interface RoutingPolicy {
  model_map_id: string; mode: RoutingMode; simple_provider_ids: string[]; complex_provider_ids: string[];
}
export interface RoutingConfig {
  revision: string; classifier: RoutingClassifier | null; policies: RoutingPolicy[];
  audit: Array<{revision: string; previous_revision: string; reason: string; created_at_secs: number}>;
}
export interface RoutingDecision {
  invocation_id: string; scope: string; request_hash: string; revision: string; model_map_id: string;
  mode: RoutingMode; complexity: 'simple' | 'complex' | 'unknown'; reason: string; provider_ids: string[]; served_provider_id: string | null;
  created_at_secs: number; pending: boolean; classifier_attempted: boolean; classifier_latency_ms: number | null;
  input_tokens: number; output_tokens: number; classifier_cost_micro_cny: number; usage_estimated: boolean; preview: boolean;
}
export interface RoutingStatus {
  retained_decisions?: number;
  budget: {day: number; calls: number; cost_micro_cny: number}; recent_decisions: RoutingDecision[];
}
export interface RoutingUpdate {
  expected_revision: string; reason: string; classifier: RoutingClassifier | null; policies: RoutingPolicy[];
}
export interface RoutingPreviewInput {model_map_id: string; text: string; history?: string; continuation?: boolean; has_attachments?: boolean}
export interface RoutingPreview {success: boolean; decision: RoutingDecision; eligible_provider_ids: string[]; applied_provider_ids: string[]}
// Keep money as decimal text while editing. Multiplying a floating-point yuan value can silently round a micro-yuan.
export function yuanToMicro(text: string): number | null {
  const value = text.trim();
  if (value.length > 32 || !/^\d+(?:\.\d{1,6})?$/.test(value)) return null;
  const [whole, fraction = ''] = value.split('.');
  const micro = BigInt(whole) * 1000000n + BigInt(fraction.padEnd(6, '0'));
  return micro <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(micro) : null;
}
export function microToYuan(value: number): string {
  if (!Number.isSafeInteger(value) || value < 0) return '—';
  const micro = BigInt(value), fraction = String(micro % 1000000n).padStart(6, '0').replace(/0+$/, '');
  return `${micro / 1000000n}${fraction ? '.' + fraction : ''}`;
}
export interface ClassifierDraft {
  provider_id: string; model: string; timeout_ms: string; max_input_chars: string; daily_request_limit: string;
  daily_budget_yuan: string; input_price_yuan: string; output_price_yuan: string;
}
export interface RoutingDraft {classifier: ClassifierDraft | null; policies: RoutingPolicy[]}
export const newClassifier = (): ClassifierDraft => ({provider_id: '', model: '', timeout_ms: '1500', max_input_chars: '4096',
  daily_request_limit: '1000', daily_budget_yuan: '', input_price_yuan: '', output_price_yuan: ''});
export function routingDraft(config: RoutingConfig): RoutingDraft {
  const c = config.classifier;
  return {classifier: c ? {provider_id: c.provider_id, model: c.model, timeout_ms: String(c.timeout_ms), max_input_chars: String(c.max_input_chars),
    daily_request_limit: String(c.daily_request_limit), daily_budget_yuan: microToYuan(c.daily_budget_micro_cny),
    input_price_yuan: microToYuan(c.input_price_micro_cny_per_million), output_price_yuan: microToYuan(c.output_price_micro_cny_per_million)} : null,
    policies: config.policies.map(p => ({...p, simple_provider_ids: [...p.simple_provider_ids], complex_provider_ids: [...p.complex_provider_ids]}))};
}
const integerIn = (text: string, min: number, max: number) => /^\d+$/.test(text) && Number.isSafeInteger(Number(text)) && Number(text) >= min && Number(text) <= max;
/** Same primary + fallbacks as ModelMap.full_target_chain(); never infer a route from price. */
export function routingProviders(model: Row | undefined, providers: Row[]): Row[] {
  const ids = new Set(model ? targetsOf(model).map(target => target.provider_id) : []);
  return providers.filter(provider => provider.enabled !== false && ids.has(String(provider.id)));
}
const cleanId = (text: string) => !!text.trim() && !/[\u0000-\u001f\u007f-\u009f]/.test(text);
export function routingError(draft: RoutingDraft, models: Row[], providers: Row[]): string {
  const c = draft.classifier;
  if (c) {
    if (!providers.some(p => p.id === c.provider_id && p.enabled !== false)) return '请选择已有且已启用的分类上游';
    if (!cleanId(c.model)) return '请填写分类器的上游模型 ID，不可含控制字符';
    if (!integerIn(c.timeout_ms, 200, 5000)) return '分类超时须为 200–5000 毫秒的整数';
    if (!integerIn(c.max_input_chars, 256, 16000)) return '分类上下文上限须为 256–16000 个字符的整数';
    if (!integerIn(c.daily_request_limit, 1, 100000)) return '每日分类调用上限须为 1–100000 的整数';
    if ([c.daily_budget_yuan, c.input_price_yuan, c.output_price_yuan].some(v => yuanToMicro(v) === null)) return '请填写每日预算及输入、输出价格：非负十进制金额，最多 6 位小数，不使用科学计数法';
    for (const [label, value] of [['每日预算', c.daily_budget_yuan], ['输入价格', c.input_price_yuan], ['输出价格', c.output_price_yuan]]) {
      const micro = yuanToMicro(value)!;
      if (micro < 1 || micro > 1000000000000) return `${label}须为 0.000001–1000000 元（大于 0，最多 6 位小数）`;
    }
  }
  if (draft.policies.length > 256) return '最多配置 256 个模型策略';
  const seen = new Set<string>();
  for (const p of draft.policies) {
    const model = models.find(m => m.id === p.model_map_id);
    if (!model || model.retired === true) return '策略引用的模型已不存在或已下架，请重新选择或移除策略';
    if (seen.has(p.model_map_id)) return '每个模型只能配置一条策略';
    seen.add(p.model_map_id);
    if (!['off', 'observe', 'enforce'].includes(p.mode)) return '请选择有效的策略模式';
    if (p.mode !== 'off' && !c) return '观察或执行模式需要先配置分类器';
    for (const [label, ids] of [['简单', p.simple_provider_ids], ['复杂', p.complex_provider_ids]] as const) {
      if (!ids.length) return `${label}链至少选择一个已有上游（关闭模式也需完整配置）`;
      if (ids.length > 16) return `${label}链最多选择 16 个上游`;
      if (new Set(ids).size !== ids.length) return `${label}链不能重复选择同一上游`;
      if (ids.some(id => !routingProviders(model, providers).some(provider => provider.id === id))) return `${label}链仅可选择该模型完整目标链中的已启用上游，请重新选择或移除`;
    }
  }
  return '';
}
export function routingUpdate(draft: RoutingDraft, revision: string, reason: string): RoutingUpdate {
  const c = draft.classifier;
  return {expected_revision: revision, reason: reason.trim(), classifier: c ? {provider_id: c.provider_id, model: c.model.trim(),
    timeout_ms: Number(c.timeout_ms), max_input_chars: Number(c.max_input_chars), daily_request_limit: Number(c.daily_request_limit),
    daily_budget_micro_cny: yuanToMicro(c.daily_budget_yuan)!, input_price_micro_cny_per_million: yuanToMicro(c.input_price_yuan)!,
    output_price_micro_cny_per_million: yuanToMicro(c.output_price_yuan)!} : null,
    policies: draft.policies.map(p => ({...p, simple_provider_ids: [...p.simple_provider_ids], complex_provider_ids: [...p.complex_provider_ids]}))};
}
export function shutdownUpdate(config: RoutingConfig): RoutingUpdate {
  return {expected_revision: config.revision, reason: '管理员快速关停全部智能分流', classifier: config.classifier,
    policies: config.policies.map(p => ({...p, mode: 'off'}))};
}
export function moveProvider(ids: string[], index: number, direction: -1 | 1): string[] {
  const next = [...ids], to = index + direction;
  if (index >= 0 && index < next.length && to >= 0 && to < next.length) [next[index], next[to]] = [next[to], next[index]];
  return next;
}
export const modeLabel: Record<RoutingMode, string> = {off: '关闭', observe: '观察（不改变回答路由）', enforce: '执行（应用分流顺序）'};
export const complexityLabel = {simple: '简单', complex: '复杂', unknown: '不确定（走复杂链）'};
const reasons: Record<string, string> = {
  default_route: '使用模型原有默认路由',
  classifier_unconfigured: '未配置分类器，使用默认路由',
  disabled: '分流已关闭，使用原路由',
  capability_required: '请求需要特定能力，采用复杂链或默认路由',
  continuation_without_state: '续写缺少可复用状态，采用复杂链或默认路由',
  task_continuation: '同一任务续写，沿用已有路由状态',
  insufficient_context: '上下文过长或不完整，不额外截断分类，采用复杂链或默认路由',
  empty_task: '没有可分类的任务文本，使用默认路由',
  classifier_circuit_open: '分类器熔断保护中，采用复杂链或默认路由',
  classifier_unavailable: '分类器不可用，采用复杂链或默认路由',
  classifier_busy: '分类器繁忙，采用复杂链或默认路由',
  classification_pending: '分类尚未完成，采用复杂链或默认路由',
  decision_capacity_exhausted: '分流记录已达容量上限，停止新增分类调用，采用复杂链或原路由',
  no_eligible_route: '建议链没有当前可用渠道；观察模式保留原路由，执行模式拒绝越权降级',
  simple_route_unavailable: '简单链暂无可用渠道，采用复杂链',
  continuation_route_unavailable: '上次服务链已不可用，采用当前复杂链',
  budget_exhausted: '当日分类调用或运营预算已达上限，采用复杂链或默认路由',
  semantic_simple: '语义分类判断为简单请求',
  semantic_complex: '语义分类判断为复杂请求',
  semantic_uncertain: '语义分类无法确定，按复杂请求处理',
  classifier_timeout: '分类超时，采用复杂链或默认路由',
  classifier_transport_error: '分类上游连接失败，采用复杂链或默认路由',
  classifier_invalid_input: '分类输入无效，采用复杂链或默认路由',
  classifier_invalid_response: '分类响应无法识别，采用复杂链或默认路由',
  classifier_http_error: '分类上游返回请求错误，采用复杂链或默认路由',
};
export const decisionReason = (reason: string) => reasons[reason] ?? '服务器返回了未识别原因，请结合模式与结果人工核查';
export function budgetDay(day: number): string {
  if (day === 0) return '尚未开始';
  const date = new Date(day * 86400000);
  return Number.isSafeInteger(day) && Number.isFinite(date.getTime()) ? date.toISOString().slice(0, 10) + '（UTC）' : '日期不可用';
}
export function decisionDistribution(decisions: RoutingDecision[]) {
  const counts = {simple: 0, complex: 0, unknown: 0};
  for (const d of decisions) if (!d.pending && d.classifier_attempted && !d.preview) counts[d.complexity]++;
  return counts;
}
const object = (value: unknown): value is Record<string, unknown> => !!value && typeof value === 'object' && !Array.isArray(value);
const natural = (value: unknown): value is number => Number.isSafeInteger(value) && Number(value) >= 0;
const text = (value: unknown): value is string => typeof value === 'string' && !!value.trim();
const ids = (value: unknown): value is string[] => Array.isArray(value) && value.every(text);
const mode = (value: unknown) => value === 'off' || value === 'observe' || value === 'enforce';
export function validRoutingConfig(value: unknown): value is RoutingConfig {
  if (!object(value) || !text(value.revision) || !Array.isArray(value.policies) || !Array.isArray(value.audit)) return false;
  const c = value.classifier;
  if (c !== null && (!object(c) || !text(c.provider_id) || !text(c.model) ||
    !['timeout_ms', 'max_input_chars', 'daily_request_limit', 'daily_budget_micro_cny', 'input_price_micro_cny_per_million', 'output_price_micro_cny_per_million'].every(key => natural(c[key])))) return false;
  return value.policies.every(p => object(p) && text(p.model_map_id) && mode(p.mode) && ids(p.simple_provider_ids) && ids(p.complex_provider_ids)) &&
    value.audit.every(a => object(a) && text(a.revision) && typeof a.previous_revision === 'string' && text(a.reason) && natural(a.created_at_secs));
}
export function validDecision(d: unknown): d is RoutingDecision {
  return object(d) && ['invocation_id', 'scope', 'request_hash', 'revision', 'model_map_id', 'reason'].every(key => text(d[key])) && mode(d.mode) &&
    ['simple', 'complex', 'unknown'].includes(String(d.complexity)) && ids(d.provider_ids) &&
    (d.served_provider_id === null || text(d.served_provider_id)) &&
    ['created_at_secs', 'input_tokens', 'output_tokens', 'classifier_cost_micro_cny'].every(key => natural(d[key])) &&
    (d.classifier_latency_ms === null || natural(d.classifier_latency_ms)) &&
    ['pending', 'classifier_attempted', 'usage_estimated', 'preview'].every(key => typeof d[key] === 'boolean');
}
export function validRoutingStatus(s: unknown): s is RoutingStatus {
  return object(s) && (s.retained_decisions === undefined || natural(s.retained_decisions)) && object(s.budget) && ['day', 'calls', 'cost_micro_cny'].every(key => natural(s.budget && (s.budget as Record<string, unknown>)[key])) &&
    Array.isArray(s.recent_decisions) && s.recent_decisions.every(validDecision);
}
export function validPreview(p: unknown): p is RoutingPreview {
  return object(p) && p.success === true && validDecision(p.decision) && p.decision.preview && ids(p.eligible_provider_ids) && ids(p.applied_provider_ids);
}
