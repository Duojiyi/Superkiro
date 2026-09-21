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

// Exercise the component's real event handlers without a browser or extra dependencies.
const fields = ['fixed_input_credit_per_m', 'fixed_output_credit_per_m', 'fixed_cache_creation_credit_per_m', 'fixed_cache_read_credit_per_m'];
const model = {id: 'model-1', exposed_model_id: 'test-model', target_model: 'upstream', group_id: 'g', credit_multiplier: 4};
const group = {id: 'g', rate_card_id: 'r', margin_multiplier: 3};
const version = {id: 'new-price', model: 'test-model', rate_card_id: 'r', pricing_mode: 'fixed', margin_multiplier: 2, effective_from_secs: 2000000000, currency: 'USD', input_price_per_m: 99, output_price_per_m: 99, cache_read_price_per_m: 99, cache_creation_price_per_m: 99};
let states, cursor;
const jsx = (type, props) => ({type, props});
const Editor = load('CommercialEditor.tsx', {'./tokens': load('tokens.ts'), './pricing': pricing, './api': {adminApi: {}}, react: {
  useEffect: () => {}, useRef: initial => ({current: initial}), useState: initial => {const index = cursor++; if (!(index in states)) states[index] = initial; return [states[index], value => {states[index] = typeof value === 'function' ? value(states[index]) : value;}];},
}, 'react/jsx-runtime': {jsx, jsxs: jsx}}).default;
const render = () => {cursor = 0; return Editor({kind: 'models', onDirtyChange: () => {}, onBusyChange: () => {}});};
function nodes(node) {if (!node || typeof node !== 'object') return []; if (Array.isArray(node)) return node.flatMap(nodes); return [node, ...nodes(node.props?.children)];}
function text(node) {if (node == null || typeof node === 'boolean') return ''; if (Array.isArray(node)) return node.map(text).join(''); return typeof node === 'object' ? text(node.props?.children) : String(node);}
const find = (tree, type, label) => nodes(tree).find(node => node.type === type && text(node).startsWith(label));
function setup() {
  const draft = JSON.stringify({models: [model], rate_cards: [], versions: []});
  states = [{groups: [group], versions: []}, draft, draft, '', '', false, 'model-1', {...version}, Object.fromEntries(fields.map((field, i) => [field, String(i + 1)])), 'million', ['1000', '1000', '1000', '1000']];
  return render();
}
let tree = setup();
assert(text(tree).includes('预计扣费：0.24 积分（240000 微积分）'));
assert(text(tree).includes('不参与固定模式客户扣费'));
let unit = nodes(find(tree, 'label', '客户售价单位')).find(node => node.type === 'select');
unit.props.onChange({target: {value: 'thousand'}});
assert.equal(states[8][fields[0]], '0.001');
tree = render();
assert(text(tree).includes('预计扣费：0.24 积分'));
find(tree, 'button', '加入价格草稿').props.onClick();
const staged = JSON.parse(states[1]).versions[0];
assert.equal(staged.fixed_input_credit_per_m, 1000000);
assert.equal(staged.input_price_per_m, 99); // procurement is not overwritten or used as customer price
assert.equal(staged.margin_multiplier, 2);
assert.equal(states[7], null);
tree = setup(); states[8][fields[0]] = '';
unit = nodes(find(render(), 'label', '客户售价单位')).find(node => node.type === 'select');
unit.props.onChange({target: {value: 'thousand'}});
assert.equal(states[9], 'million');
assert(states[4].includes('切换单位前请修正售价'));
setup(); states[7].rate_card_id = 'other'; assert(text(render()).includes('价格表与此草稿不匹配'));
setup(); states[7].margin_multiplier = ''; find(render(), 'button', '加入价格草稿').props.onClick(); assert(states[4].includes('倍率必须为非负有限数'));
console.log('Pricing units, precision, charge preview and editor staging tests passed');

// Existing datetime-local input submits epoch seconds, not a hand-entered Unix value.
setup();
const dateInput = nodes(find(render(), 'label', '生效时间（本地时区）')).find(node => node.type === 'input');
assert.equal(dateInput.props.type, 'datetime-local');
dateInput.props.onChange({target: {value: '2030-01-01T10:00'}});
assert.equal(states[7].effective_from_secs, Math.floor(new Date('2030-01-01T10:00').getTime() / 1000));
find(render(), 'button', '加入价格草稿').props.onClick();
assert.equal(JSON.parse(states[1]).versions[0].effective_from_secs, Math.floor(new Date('2030-01-01T10:00').getTime() / 1000));
setup();
states[0].versions = [
  {...version, id: 'fixed-history', ...Object.fromEntries(fields.map(field => [field, 1234567]))},
  {...version, id: 'call-history', pricing_mode: 'per_call', per_call_credit: 250000},
  {...version, id: 'cost-history', pricing_mode: 'cost_plus'},
  {...version, id: 'invalid-history', fixed_input_credit_per_m: Number.MAX_SAFE_INTEGER + 1, effective_from_secs: null},
];
const before = JSON.stringify(states[0]);
const history = nodes(render()).find(node => node.props?.['aria-label'] === '历史价格版本');
assert(history);
assert(text(history).includes('固定积分 · 积分 / 百万 Tokens'));
assert(text(history).includes('未缓存输入：1.234567'));
assert(text(history).includes('按次计费：0.25 积分 / 次'));
assert(text(history).includes('成本加成（非固定积分售价）'));
assert(text(history).includes('采购输入价格：99'));
assert(text(history).includes(new Date(version.effective_from_secs * 1000).toLocaleString()));
assert(text(history).includes('无法安全显示，请核对原始配置'));
assert(text(history).includes('未提供有效时间'));
assert.equal(nodes(history).filter(node => ['input', 'select', 'textarea'].includes(node.type)).length, 0);
assert.equal(JSON.stringify(states[0]), before);
console.log('Local date conversion and read-only human-readable historical prices passed');

// Token inputs are explicit decimal units, without model-name inference or a 32K cap.
const tokens = load('tokens.ts');
for (const [input, expected] of [['128K',128000],['1M',1000000],['1.001K',1001],['0.000001M',1],['64k',64000],['200000',200000]]) assert.equal(tokens.parseTokenInput(input),expected);
for (const input of ['', '-1','0','1.1','1e6','NaN','Infinity','1.0001K','9007199254740992']) assert.equal(tokens.parseTokenInput(input),input);
assert.equal(tokens.formatTokens(200000),'200K Tokens（200,000）');
assert.equal(tokens.formatTokens(1000000),'1M Tokens（1,000,000）');
setup();
states[1] = JSON.stringify({models: [{...model,context_window:200000,max_output:64000},{...model,id:'model-2',context_window:128000,max_output:32000}], versions:[]});
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

setup();
states[1] = JSON.stringify({models:[{...model,context_window:200000},{...model,context_window:128000}]});
const duplicateDraft = states[1];
field('上下文长度').props.onChange({target:{value:'1M'}});
assert.equal(states[1],duplicateDraft,'duplicate IDs cannot cause multiple rows to be edited');
assert(states[4].includes('ID 重复或不存在'));
