const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require('typescript');
function load(file, imports = {}) {
  const exports = {};
  const code = ts.transpileModule(fs.readFileSync(path.join(__dirname, '../src', file), 'utf8'), {
    compilerOptions: {module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020, jsx: ts.JsxEmit.ReactJSX},
  }).outputText;
  vm.runInNewContext(code, {exports, require: name => imports[name], TextEncoder, window: {confirm: () => true}});
  return exports;
}
const pricing = load('pricing.ts');
const {pointsToMicro, adjustmentPointsToMicro, priceToMicroPerMillion: parse, formatMicroPrice: format, previewFixedCharge: charge} = pricing;
for (const micro of [0, 1, 1000001, 123456789, Number.MAX_SAFE_INTEGER]) {
  for (const unit of ['million', 'thousand']) assert.equal(parse(format(micro, unit), unit), micro);
  assert.equal(pointsToMicro(format(micro)), micro);
}
assert.equal(parse('0.12', 'thousand'), parse('120', 'million'));
assert.equal(parse('0.000000001', 'thousand'), 1);
assert.equal(adjustmentPointsToMicro('-0.000001'), -1);
for (const text of ['', '-1', '1e3', 'NaN', 'Infinity', ' 1', '1.0000001', '9007199254.740992']) assert.throws(() => parse(text, 'million'));
assert.throws(() => parse('0.0000000001', 'thousand'));
assert.throws(() => format(1.5));
assert.throws(() => pointsToMicro('9007199254.740992'));
assert.equal(charge([1000000, 2000000, 3000000, 4000000], ['1000', '1000', '1000', '1000'], [2, 3, 4]), 240000);
assert.equal(charge([1, 1, 1, 1], ['1', '1', '1', '1'], [1, 1, 1]), 1); // round once, not per category
assert.equal(charge([1000000, 0, 0, 0], ['1', '0', '0', '0'], [1.2, 1.5, 2]), 4);
assert.equal(charge([1, 1, 1, 1], ['1', '1', '1', '1'], [0, 1, 1]), 0);
for (const tokens of [['', '0', '0', '0'], ['-1', '0', '0', '0'], ['1.1', '0', '0', '0']]) assert.throws(() => charge([1, 1, 1, 1], tokens, [1, 1, 1]));
assert.throws(() => charge([Number.MAX_SAFE_INTEGER, 0, 0, 0], ['2', '0', '0', '0'], [1, 1, 1]));
assert.throws(() => charge([1, 1, 1, 1], ['1', '1', '1', '1'], [Infinity, 1, 1]));

// One-step price changes: pure rules in priceChange.ts, without a browser.
const display = load('format.ts');
const change = load('priceChange.ts', {'./pricing': pricing});
const now = Math.floor(new Date('2026-09-26T10:00:00').getTime() / 1000);
const versions = [
  {id: 'v-old', model: 'test-model', rate_card_id: 'r', pricing_mode: 'fixed', effective_from_secs: now - 200, fixed_input_credit_per_m: 1000000},
  {id: 'v-now', model: 'test-model', rate_card_id: 'r', pricing_mode: 'fixed', effective_from_secs: now - 100, fixed_input_credit_per_m: 2000000},
  {id: 'v-next', model: 'test-model', rate_card_id: 'r', pricing_mode: 'fixed', effective_from_secs: now + 100},
  {id: 'v-upstream', model: 'upstream', rate_card_id: 'r', pricing_mode: 'fixed', effective_from_secs: now - 300},
  {id: 'v-star', model: '*', rate_card_id: 'r', pricing_mode: 'cost_plus', effective_from_secs: now - 400},
  {id: 'v-other-card', model: 'test-model', rate_card_id: 'other', pricing_mode: 'fixed', effective_from_secs: now - 50},
];
// Resolved like billing: the model's own price, else its upstream model's, else the card's '*'.
assert.equal(change.currentVersion(versions, 'r', ['test-model', 'upstream'], now).id, 'v-now');
assert.equal(change.currentVersion(versions, 'r', ['unpriced', 'upstream'], now).id, 'v-upstream');
assert.equal(change.currentVersion(versions, 'r', ['unpriced', 'nothing'], now).id, 'v-star');
assert.equal(change.currentVersion(versions, 'missing', ['test-model'], now), null);
assert.deepEqual(change.scheduledVersions(versions, 'r', ['test-model'], now).map(v => v.id), ['v-next']);
// Version IDs: model + the minute it takes effect, never one already used, at most 128 bytes.
const at = Math.floor(new Date('2026-09-27T06:05:00').getTime() / 1000);
assert.equal(change.versionIdFor('claude-opus-5', at, []), 'claude-opus-5-202609270605');
assert.equal(change.versionIdFor('claude-opus-5', at, ['claude-opus-5-202609270605']), 'claude-opus-5-202609270605-2');
assert.equal(change.versionIdFor('claude-opus-5', at, ['claude-opus-5-202609270605', 'claude-opus-5-202609270605-2']), 'claude-opus-5-202609270605-3');
assert(new TextEncoder().encode(change.versionIdFor('模'.repeat(60), at, [])).length <= 128);
// Changes: signed percent, — when unchanged, 新 when there was no price.
assert.equal(change.percentChange(15000000, 12000000), '−20%');
assert.equal(change.percentChange(3000000, 3150000), '+5%');
assert.equal(change.percentChange(3000000, 3000000), '—');
assert.equal(change.percentChange(null, 1000000), '新');
assert.equal(change.percentChange(0, 1000000), '新');
assert.equal(change.percentChange(1000000, null), '');
// A new version: exact integer micro-credits, the procurement prices kept apart, and every guard.
const input = {prices: {fixed_input_credit_per_m: '15', fixed_output_credit_per_m: '60', fixed_cache_creation_credit_per_m: '18.75', fixed_cache_read_credit_per_m: '1.5'},
  costs: {input_price_per_m: '15', output_price_per_m: '75', cache_creation_price_per_m: '18.75', cache_read_price_per_m: '1.5'}, currency: 'USD', multiplier: '1', effectiveSecs: now + 300, id: 'test-model-new'};
const context = {model: 'test-model', rateCardId: 'r', versions, nowSecs: now, base: versions[1]};
const built = change.buildPriceVersion(input, context);
assert.equal(built.fixed_output_credit_per_m, 60000000);
assert.equal(built.fixed_cache_creation_credit_per_m, 18750000);
assert.equal(built.output_price_per_m, 75); // procurement is its own field, never a customer price
assert.equal(built.pricing_mode, 'fixed');assert.equal(built.model, 'test-model');assert.equal(built.rate_card_id, 'r');
assert.equal(built.margin_multiplier, 1);assert.equal(built.effective_from_secs, now + 300);assert.equal(built.id, 'test-model-new');
const refuses = (patch, message, extra = {}) => assert.throws(() => change.buildPriceVersion({...input, ...patch}, {...context, ...extra}), error => error.message.includes(message), message);
refuses({id: 'v-now'}, '版本 ID 已存在');
refuses({id: ' '}, '版本 ID 无效');
refuses({effectiveSecs: now}, '生效时间需晚于现在');
refuses({effectiveSecs: now + 100}, '这一时刻已有这个模型的价格版本');
refuses({multiplier: '0'}, '版本倍率需大于 0、不超过 1000');
refuses({multiplier: ''}, '版本倍率需大于 0、不超过 1000');
refuses({currency: ''}, '请选择采购价币种');
refuses({prices: {...input.prices, fixed_output_credit_per_m: '1.0000001'}}, '输出售价须为非负数，最多 6 位小数');
refuses({prices: {...input.prices, fixed_input_credit_per_m: '1000000.000001'}}, '输入售价最多 1,000,000 积分');
refuses({costs: {...input.costs, cache_read_price_per_m: ''}}, '缓存读采购价需在 0–1,000,000 之间');
// A sample: billing's exact charge, then yuan at the face value, the cost in yuan, and the margin.
const sample = change.sampleCost({rates: [3000000, 15000000, 3750000, 300000], tokens: ['1000', '500', '100', '200'], multipliers: [1.2, 1.5, 2],
  faceValueCny: 0.03, costs: [3, 15, 3.75, 0.3], currency: 'USD', usdCnyRate: 7.2});
assert.equal(sample.credits, 39366);
assert(Math.abs(sample.yuan - 0.00118098) < 1e-12);
assert(Math.abs(sample.costYuan - 0.078732) < 1e-12);
assert(Math.abs(sample.marginPct - (0.00118098 - 0.078732) / 0.00118098 * 100) < 1e-9);
assert.equal(change.sampleCost({rates: [1000000, 0, 0, 0], tokens: ['1000', '0', '0', '0'], multipliers: [1, 1, 1], faceValueCny: 0.03, costs: [1, 0, 0, 0], currency: 'CNY'}).costYuan, 0.001);
assert.equal(change.sampleCost({rates: [1000000, 0, 0, 0], tokens: ['1000', '0', '0', '0'], multipliers: [1, 1, 1], costs: [1, 0, 0, 0], currency: 'USD'}).marginPct, null, 'no margin without a face value and rate');
console.log('PASS: price resolution like billing, generated version IDs, change percentages, exact new versions with every guard, sample yuan and margin');

// Price versions: one row each; superseded ones only with 显示历史.
let states, cursor;
const jsx = (type, props) => ({type, props});
const react = {
  useEffect: () => {}, useRef: initial => ({current: initial}), useState: initial => {const index = cursor++; if (!(index in states)) states[index] = typeof initial === 'function' ? initial() : initial; return [states[index], value => {states[index] = typeof value === 'function' ? value(states[index]) : value;}];},
};
const runtime = {jsx, jsxs: jsx, Fragment: 'Fragment'};
function nodes(node) {if (!node || typeof node !== 'object') return []; if (Array.isArray(node)) return node.flatMap(nodes); return [node, ...nodes(node.props?.children)];}
function text(node) {if (node == null || typeof node === 'boolean') return ''; if (Array.isArray(node)) return node.map(text).join(''); return typeof node === 'object' ? text(node.props?.children) : String(node);}
const Versions = load('PriceVersions.tsx', {'./components/ui': {IdCell: 'IdCell', StatusBadge: 'StatusBadge'}, './format': display, './priceChange': change, './status': load('status.ts'), react, 'react/jsx-runtime': runtime}).default;
const history = [
  {id: 'fixed-history', model: 'test-model', rate_card_id: 'r', pricing_mode: 'fixed', effective_from_secs: now - 200, fixed_input_credit_per_m: 1234567, fixed_output_credit_per_m: 1, fixed_cache_creation_credit_per_m: 1, fixed_cache_read_credit_per_m: 1, currency: 'USD', input_price_per_m: 99, output_price_per_m: 99, cache_read_price_per_m: 99, cache_creation_price_per_m: 99, margin_multiplier: 2},
  {id: 'fixed-now', model: 'test-model', rate_card_id: 'r', pricing_mode: 'fixed', effective_from_secs: now - 100, fixed_input_credit_per_m: 2000000, fixed_output_credit_per_m: Number.MAX_SAFE_INTEGER + 1, fixed_cache_creation_credit_per_m: 1, fixed_cache_read_credit_per_m: 1},
  {id: 'call-price', model: 'call-model', rate_card_id: 'r', pricing_mode: 'per_call', per_call_credit: 250000, effective_from_secs: now - 100},
  {id: 'cost-price', model: 'cost-model', rate_card_id: 'r', pricing_mode: 'cost_plus', effective_from_secs: now - 100},
  {id: 'no-time', model: 'odd-model', rate_card_id: 'r', pricing_mode: 'fixed', effective_from_secs: null},
];
const renderVersions = () => {cursor = 0; return Versions({versions: history, rateCardName: id => `卡 ${id}`, nowSecs: now});};
states = [];
let table = renderVersions();
const badges = tree => nodes(tree).filter(node => node.type === 'StatusBadge').map(node => node.props.view.label);
assert(!badges(table).includes('已被替代'), 'superseded versions are hidden by default');
assert(text(table).includes('显示历史（1）'));
assert(text(table).includes('按次 0.25 积分'));
assert(text(table).includes('成本加成'));
assert(text(table).includes('未提供有效时间'));
assert(nodes(table).some(node => node.props?.title === '无法安全显示，请核对原始配置'), 'an unsafe number is flagged, not shown');
nodes(table).find(node => node.type === 'input' && node.props.type === 'checkbox').props.onChange({target: {checked: true}});
table = renderVersions();
assert(badges(table).includes('已被替代'));
assert(text(table).includes('1.234567'));
assert(text(table).includes('USD 99 / 99 / 99 / 99'));
assert(text(table).includes(display.formatDateTime(now - 200)));
assert.equal(nodes(table).filter(node => ['select', 'textarea'].includes(node.type)).length, 0, 'the version list is read-only');
console.log('PASS: one row per version, 显示历史, per-call / cost-plus / unsafe / missing values shown plainly');

// Token inputs are explicit decimal units, without model-name inference or a 32K cap.
const tokens = load('tokens.ts');
for (const [input, expected] of [['128K',128000],['1M',1000000],['1.001K',1001],['0.000001M',1],['64k',64000],['200000',200000],['272K',272000]]) assert.equal(tokens.parseTokenInput(input),expected);
for (const input of ['', '-1','0','1.1','1e6','NaN','Infinity','1.0001K','9007199254740992']) assert.equal(tokens.parseTokenInput(input),input);
assert.equal(tokens.formatTokens(200000),'200K Tokens（200,000）');
assert.equal(tokens.formatTokens(1000000),'1M Tokens（1,000,000）');
assert.equal(display.formatTokenCount(272000),'272K');
const model = {id: 'model-1', exposed_model_id: 'test-model', target_model: 'upstream', target_provider_id: 'p', group_id: 'g', credit_multiplier: 4};
const Editor = load('CommercialEditor.tsx', {'./tokens': tokens, './api': {adminApi: {}}, './format': display, './priceChange': change,
  // Dialogs, drawers and messages only run from event handlers; the render tree just names the components.
  './components/confirm': {confirmAction: async () => true}, './components/toast': {toast: {success() {}, info() {}, error() {}}},
  './components/modal': {Drawer: 'Drawer', Modal: 'Modal'}, './components/icons': {IconImage: 'IconImage', IconSpark: 'IconSpark', IconTool: 'IconTool'},
  './components/ui': {InfoTip: 'InfoTip', TopbarActions: 'TopbarActions'}, './PriceDrawer': {default: 'PriceDrawer'}, './PriceVersions': {default: 'PriceVersions'},
  react, 'react/jsx-runtime': runtime}).default;
const providers = [{id: 'p', name: '供应商 P'}, {id: 'openai', name: 'Astra', api_type: 'openai'}];
const providerKeys = [{id: 'k', provider_id: 'openai', allowed_models: ['gpt-6-astra', 'gpt-5.6-sol']}, {id: 'k2', provider_id: 'p', allowed_models: ['upstream']}];
const render = () => {cursor = 0; return Editor({kind: 'models', onDirtyChange: () => {}, onBusyChange: () => {}, providers, providerKeys});};
function setup(rows = [model]) {
  const draft = JSON.stringify({models: rows, rate_cards: [], versions: []});
  states = [{groups: [{id: 'g', rate_card_id: 'r', margin_multiplier: 3}], models: rows, versions: [], rate_cards: []}, draft, draft, '', '', false, 'model-1'];
  return render();
}
setup([{...model,context_window:200000,max_output:64000},{...model,id:'model-2',context_window:128000,max_output:32000}]);
const field = name => nodes(render()).find(node => node.type === 'input' && node.props['aria-label'] === name);
assert.equal(field('最大输出').props.value,'64000');
field('上下文长度').props.onChange({target:{value:'1M'}});
assert.equal(JSON.parse(states[1]).models[0].context_window,1000000);
const reordered = JSON.parse(states[1]); reordered.models.reverse(); states[1] = JSON.stringify(reordered);
assert.equal(field('上下文长度').props.value,'1000000','selection follows ID after JSON reorder');
field('最大输出').props.onChange({target:{value:'128K'}});
assert.equal(JSON.parse(states[1]).models[1].max_output,128000);
assert.equal(JSON.parse(states[1]).models[0].max_output,32000,'other model remains untouched');
field('上下文长度').props.onChange({target:{value:''}});
assert.equal(field('上下文长度').props.value,'');
states[1] = JSON.stringify({models:[reordered.models[0]]});
assert(!field('上下文长度'),'removed selection must not silently edit another model');
console.log('PASS: decimal K/M tokens, original values, uncapped output, invalid drafts and stable model selection');

// Routing is picked from lists: providers, and the chosen provider's authorised models (custom allowed).
setup();
const select = nodes(render()).find(node => node.type === 'select' && node.props['aria-label'] === '供应商');
assert.deepEqual(nodes(select).filter(node => node.type === 'option').map(node => node.props.value), ['p', 'openai']);
assert(text(select).includes('Astra · OpenAI'));
select.props.onChange({target: {value: 'openai'}});
assert.equal(JSON.parse(states[1]).models[0].target_provider_id, 'openai');
let tree = render();
const upstream = nodes(tree).find(node => node.type === 'input' && node.props['aria-label'] === '上游模型');
const list = nodes(tree).find(node => node.type === 'datalist' && node.props.id === upstream.props.list);
assert.deepEqual(nodes(list).filter(node => node.type === 'option').map(node => node.props.value), ['gpt-5.6-sol', 'gpt-6-astra']);
assert(text(tree).includes('这个供应商的 Key 还没有授权此模型'), 'an upstream model the provider does not serve is pointed out');
assert(text(tree).includes('原 供应商 P'), 'an edited field shows what it was');
upstream.props.onChange({target: {value: 'my-own-upstream'}});
assert.equal(JSON.parse(states[1]).models[0].target_model, 'my-own-upstream', 'a value outside the list is kept');
// An unknown provider stays selectable as it is, marked, rather than being replaced.
setup([{...model, target_provider_id: 'gone'}]);
const unknown = nodes(render()).find(node => node.type === 'select' && node.props['aria-label'] === '供应商');
assert(text(unknown).includes('gone（未找到）'));
console.log('PASS: provider dropdown, upstream model list with custom values, edited-field hints');

setup([{...model,context_window:200000},{...model,context_window:128000}]);
const duplicateDraft = states[1];
field('上下文长度').props.onChange({target:{value:'1M'}});
assert.equal(states[1],duplicateDraft,'duplicate IDs cannot cause multiple rows to be edited');
assert(states[4].includes('ID 重复或不存在'));
