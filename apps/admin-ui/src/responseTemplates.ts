import {PELICAN_OPUS55_HTML} from './pelicanTemplate';
import type {Row} from './types';

export interface ResponseTemplateVariant {
  model_id: string;
  file_path: string;
  content: string;
  preamble: string;
  completion: string;
  price_microcredits: number;
  delay_ms?: number;
}
export interface ResponseTemplateRule {
  id: string;
  name: string;
  enabled: boolean;
  match_mode: 'exact' | 'contains';
  match_text: string;
  variants: ResponseTemplateVariant[];
}
export interface ResponseTemplateConfig {revision: string; rules: ResponseTemplateRule[]; audit: unknown[]}
export interface ResponseTemplateUpdate {expected_revision: string; reason: string; rules: ResponseTemplateRule[]}
export type TemplateVariantDraft = Omit<ResponseTemplateVariant, 'price_microcredits'> & {price_credits: string};
export type TemplateRuleDraft = Omit<ResponseTemplateRule, 'variants'> & {variants: TemplateVariantDraft[]};
export const MAX_TEMPLATE_ITEMS = 32;
export const MAX_TEMPLATE_BYTES = 256 * 1024;
export const MAX_TEMPLATES_BYTES = 2 * 1024 * 1024;
const bytes = (text: string) => new TextEncoder().encode(text).length;

/** Decimal text to integer microcredits, without floating-point rounding or a paid fallback. */
export function templatePrice(text: string): number | null {
  if (!/^\d+(?:\.\d{1,6})?$/.test(text)) return null;
  const [whole, fraction = ''] = text.split('.');
  const value = Number(whole) * 1_000_000 + Number(fraction.padEnd(6, '0'));
  return Number.isSafeInteger(value) && value <= 1_000_000_000 ? value : null;
}
export function templateCredits(micro: number): string {
  return `${Math.floor(micro / 1_000_000)}.${String(micro % 1_000_000).padStart(6, '0')}`.replace(/\.?0+$/, '');
}
export function templateDrafts(rules: ResponseTemplateRule[]): TemplateRuleDraft[] {
  return rules.map(rule => ({...rule, variants: rule.variants.map(({price_microcredits, ...variant}) => ({...variant, delay_ms: variant.delay_ms ?? 0, price_credits: templateCredits(price_microcredits)}))}));
}
export function newTemplateVariant(): TemplateVariantDraft {
  return {delay_ms: 1500, model_id: '', file_path: 'output.html', content: '', preamble: '这是固定模板服务，不是上游模型生成，不消耗或虚构上游 tokens。', completion: '模板文件生成指令已发送；服务费按下发收取一次，成功或失败回执均免费。实际文件写入取决于客户端兼容工具的执行结果。', price_credits: '0'};
}
export function newTemplateRule(): TemplateRuleDraft {
  return {id: crypto.randomUUID(), name: '新规则', enabled: false, match_mode: 'exact', match_text: '', variants: [newTemplateVariant()]};
}
/** No URL, traversal, drive, UNC, encoded separator, or Windows device/file alias. */
export function safeTemplatePath(path: string): boolean {
  return bytes(path) <= 240 && /\.html?$/i.test(path) && !/[\\:%?#<>"|*\x00-\x1f\x7f-\x9f]/.test(path) && path.split('/').every(part =>
    !!part && part !== '.' && part !== '..' && part.trim() === part && !part.endsWith('.') && !/^(con|prn|aux|nul|conin\$|conout\$|com[1-9¹²³]|lpt[1-9¹²³])$/i.test(part.split('.')[0].trimEnd()));
}
export function templateModels(models: Row[]): Array<{id: string; label: string}> {
  const unique = new Map<string, string>();
  for (const model of models) {
    if (typeof model.exposed_model_id !== 'string' || !model.exposed_model_id.trim()) continue;
    unique.set(model.exposed_model_id, typeof model.display_name === 'string' && model.display_name ? `${model.display_name} · ${model.exposed_model_id}` : model.exposed_model_id);
  }
  return [...unique].map(([id, label]) => ({id, label}));
}
export function parseTemplateRules(drafts: TemplateRuleDraft[]): {rules: ResponseTemplateRule[]; error?: never} | {error: string; rules?: never} {
  if (drafts.length > MAX_TEMPLATE_ITEMS) return {error: '最多配置 32 条规则。'};
  const ids = new Set<string>();
  const rules: ResponseTemplateRule[] = [];
  for (const [index, draft] of drafts.entries()) {
    const prefix = `规则 ${index + 1}`;
    if (!draft.id.trim() || bytes(draft.id) > 128 || /[\x00-\x1f\x7f-\x9f]/.test(draft.id) || ids.has(draft.id)) return {error: `${prefix}的 ID 为空或重复。`};
    ids.add(draft.id);
    if (!draft.name.trim() || !draft.match_text.trim()) return {error: `${prefix}请填写名称和匹配文本。`};
    if (bytes(draft.name) > 256 || /[\x00-\x1f\x7f-\x9f]/.test(draft.name)) return {error: `${prefix}名称不能超过 256 字节或包含控制字符。`};
    if (bytes(draft.match_text) > 4096 || /[\x00-\x08\x0b\x0c\x0e-\x1f\x7f-\x9f]/.test(draft.match_text)) return {error: `${prefix}匹配文本不能超过 4096 字节或包含非法控制字符。`};
    if (!['exact', 'contains'].includes(draft.match_mode)) return {error: `${prefix}请选择完整匹配或包含匹配。`};
    if (!draft.variants.length || draft.variants.length > MAX_TEMPLATE_ITEMS) return {error: `${prefix}需要 1–32 个模型变体。`};
    const models = new Set<string>();
    const variants: ResponseTemplateVariant[] = [];
    for (const [vi, variant] of draft.variants.entries()) {
      const at = `${prefix} / 变体 ${vi + 1}`;
      if (!variant.model_id.trim() || models.has(variant.model_id)) return {error: `${at}请选择模型；同一规则不能重复选择同一模型。`};
      if (!/^[A-Za-z0-9_.:/-]{1,128}$/.test(variant.model_id)) return {error: `${at}模型 ID 无效，请从商业模型目录重选。`};
      models.add(variant.model_id);
      if (!Number.isInteger(variant.delay_ms ?? 0) || (variant.delay_ms ?? 0) < 0 || (variant.delay_ms ?? 0) > 30000) return {error: at + '等待时间须为 0–30000 毫秒。'};
      if ([variant.preamble, variant.completion].some(text => bytes(text) > 4096 || /[\x00-\x08\x0b\x0c\x0e-\x1f\x7f-\x9f]/.test(text))) return {error: `${at}前置和完成消息各不能超过 4096 字节或包含非法控制字符。`};
      if (!safeTemplatePath(variant.file_path)) return {error: `${at}请使用安全的相对 .html 路径，例如 pages/demo.html，不含 ..、反斜杠或绝对路径。`};
      if (!variant.content.trim()) return {error: `${at}请填写完整 HTML 代码。`};
      if (bytes(variant.content) > MAX_TEMPLATE_BYTES) return {error: `${at}代码不能超过 256 KiB（UTF-8）。`};
      const price = templatePrice(variant.price_credits);
      if (price === null) return {error: `${at}服务费须为 0–1000 credits，最多 6 位小数；不会自动改为收费价格。`};
      const {price_credits: _price, ...rest} = variant;
      variants.push({...rest, price_microcredits: price});
    }
    rules.push({...draft, variants});
  }
  if (bytes(JSON.stringify(rules)) > MAX_TEMPLATES_BYTES) return {error: '规则总大小不能超过 2 MiB（按 UTF-8 JSON 计算）。'};
  return {rules};
}

export function pelicanTemplateRule(): TemplateRuleDraft {
  const rule = newTemplateRule();
  return {...rule, name: '鹈鹕骑自行车 · Opus 5.5', match_mode: 'contains', match_text: '在根目录创建一个独立 HTML 文件，用 SVG 绘制一只鹈鹕骑自行车的 2D 动画。不得使用外部图片、第三方库或网络资源。', variants: [{...rule.variants[0], model_id: 'claude-opus-5-5', file_path: 'pelican-bicycle-opus5.5.html', content: PELICAN_OPUS55_HTML, delay_ms: 1500}]};
}
