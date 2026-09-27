// 调用追踪's time ranges, the scope an older server's answer is narrowed to, and the totals and
// failure breakdowns of what is shown, without a browser.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require('typescript');
function load(file, imports = {}) {
  const exports = {};
  const code = ts.transpileModule(fs.readFileSync(path.join(__dirname, '../src', file), 'utf8'), {
    compilerOptions: {module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020},
  }).outputText;
  vm.runInNewContext(code, {exports, Date, require: name => imports[name]});
  return exports;
}
const status = load('status.ts');
const query = load('traceQuery.ts', {'./status': status});
const plain = value => JSON.parse(JSON.stringify(value));
const at = (y, m, d, h = 0, min = 0) => Math.floor(new Date(y, m - 1, d, h, min).getTime() / 1000);
const now = at(2026, 9, 27, 15, 20) + 17;

// Local minutes, as a datetime-local input holds them.
assert.equal(query.minuteInput(new Date(2026, 8, 6, 8, 5)), '2026-09-06T08:05');
assert.equal(query.minuteStart('2026-09-26T08:30'), at(2026, 9, 26, 8, 30));
for (const text of ['', '2026-09-26', '2026-09-26 08:30', '2026-02-30T08:30', '2026-09-26T24:00', '2026-09-26T08:60', '26-09-26T08:30'])
  assert.equal(query.minuteStart(text), null, `not a minute: ${text}`);
// 近 1 小时 and 近 24 小时 back from now; 全部 unbounded.
assert.deepEqual(plain(query.traceBounds('hour', {from: '', to: ''}, now)), {fromSecs: now - 3600});
assert.deepEqual(plain(query.traceBounds('day', {from: '2026-09-01T00:00', to: ''}, now)), {fromSecs: now - 86400}, 'a chosen range belongs to 自定义 only');
assert.deepEqual(plain(query.traceBounds('all', {from: '', to: ''}, now)), {});
// 自定义: its first minute to the end of its last, both included; either end open when empty.
assert.deepEqual(plain(query.traceBounds('custom', {from: '2026-09-26T00:00', to: '2026-09-26T23:59'}, now)), {fromSecs: at(2026, 9, 26), toSecs: at(2026, 9, 27)});
assert.deepEqual(plain(query.traceBounds('custom', {from: '2026-09-26T08:30', to: '2026-09-26T08:30'}, now)), {fromSecs: at(2026, 9, 26, 8, 30), toSecs: at(2026, 9, 26, 8, 31)}, 'one minute');
assert.deepEqual(plain(query.traceBounds('custom', {from: '2026-09-26T08:30', to: ''}, now)), {fromSecs: at(2026, 9, 26, 8, 30)});
assert.deepEqual(plain(query.traceBounds('custom', {from: '', to: '2026-09-26T08:30'}, now)), {toSecs: at(2026, 9, 26, 8, 31)});
assert.equal(query.traceBounds('custom', {from: '2026-09-26T08:31', to: '2026-09-26T08:30'}, now), null, 'the end before the start');
assert.equal(query.traceBounds('custom', {from: '2026-09-26', to: ''}, now), null, 'an unreadable time');
assert.equal(query.spanText({from: '2026-09-26T08:30', to: '2026-09-26T18:00'}), '09-26 08:30 至 09-26 18:00');
assert.equal(query.spanText({from: '2026-09-26T08:30', to: ''}), '09-26 08:30 起');
assert.equal(query.spanText({from: '', to: '2026-09-26T18:00'}), '至 09-26 18:00');
console.log('PASS: 近 1 小时, 近 24 小时 and chosen minutes as [from, to) bounds, the last minute included, either end open; unreadable or reversed ranges are none');

// The scope: what an older server returns without reading the filters is narrowed the same way.
const trace = (id, fields) => ({id, ts: at(2026, 9, 26, 12), card_id: 'card-1', exposed_model: 'gpt-5', status: 'success', provider_id: 'astra', credits_charged: 1500000, provider_cost_micro_cny: 21000,
  attempt_chain: [{provider_id: 'astra', success: true}], ...fields});
const failedOver = trace('t2', {provider_id: 'astra', attempt_chain: [{provider_id: 'hanyue-max', success: false}, {provider_id: 'astra', success: true}]});
assert(query.involves(failedOver, 'hanyue-max'), 'a provider tried before a backup answered');
assert(query.involves(failedOver, 'astra') && !query.involves(failedOver, 'kimera'));
const bounds = {fromSecs: at(2026, 9, 26), toSecs: at(2026, 9, 27)};
assert(query.inScope(failedOver, {bounds, cardId: 'card-1', model: 'gpt-5', provider: 'hanyue-max'}));
assert(!query.inScope(trace('t3', {ts: at(2026, 9, 27)}), {bounds}), 'to is not included');
assert(query.inScope(trace('t4', {ts: at(2026, 9, 26)}), {bounds}), 'from is');
assert(!query.inScope(failedOver, {bounds: {}, cardId: 'card-2'}) && !query.inScope(failedOver, {bounds: {}, model: 'claude-opus-5-5'}));
console.log('PASS: the scope matches the server: from inclusive, to exclusive, the card, the model asked for, and a provider that answered or was attempted');

// Totals as the server counts them; failures are status error, an interrupted request is not one.
const shown = [trace('a'), failedOver, trace('b', {status: 'error', credits_charged: 0, provider_cost_micro_cny: 0}), trace('c', {status: 'client_aborted', credits_charged: 700000, provider_cost_micro_cny: 9000})];
assert.deepEqual(plain(query.traceTotals(shown)), {count: 4, failures: 1, creditsCharged: 3700000, costMicroCny: 51000});
assert.deepEqual(plain(query.traceTotals([])), {count: 0, failures: 0, creditsCharged: 0, costMicroCny: 0});
assert.equal(query.chargeTotal(1234567891), '1,234.5679');
assert.equal(query.chargeTotal(3700000), '3.7');
assert.equal(query.chargeTotal(0), '0');
// Under 失败: by model, and by each provider whose attempt failed (once per request); refusals before anything was sent apart.
const failures = [
  trace('f1', {status: 'error', exposed_model: 'claude-opus-5-5', provider_id: null, attempt_chain: [{provider_id: 'hanyue-max', success: false}, {provider_id: 'hanyue-max', success: false}]}),
  trace('f2', {status: 'error', exposed_model: 'claude-opus-5-5', provider_id: null, attempt_chain: [{provider_id: 'hanyue-max', success: false}, {provider_id: 'kimera', success: false}]}),
  trace('f3', {status: 'error', exposed_model: 'gpt-5', provider_id: null, attempt_chain: []}),
  trace('f4', {status: 'error', exposed_model: 'gpt-5', provider_id: 'astra', attempt_chain: [{provider_id: 'astra', success: true}]}),
  trace('ok', {}),
];
assert.deepEqual(plain(query.failureBreakdown(failures)), {models: [['claude-opus-5-5', 2], ['gpt-5', 2]], providers: [['hanyue-max', 2], ['astra', 1], ['kimera', 1], ['', 1]]});
// A refusal for the card or the request itself is 拒绝, not 失败: apart in the tabs, the breakdown and the count.
const refusals = [trace('r1', {status: 'error', error_class: 'insufficient_balance', attempt_chain: []}), trace('r2', {status: 'error', error_class: 'input_too_long', attempt_chain: [{provider_id: 'hanyue-max', success: false}]}),
  trace('r3', {status: 'error', error_class: 'model_not_listed', attempt_chain: []})];
assert.deepEqual(plain(query.failureBreakdown([...failures, ...refusals])), plain(query.failureBreakdown(failures)), 'refusals are no failures of a model or provider');
const mixed = [...failures, ...refusals, trace('aborted', {status: 'client_aborted'}), trace('running', {status: 'in_progress'})];
assert.deepEqual(['ALL', 'error', 'refused', 'client_aborted', 'in_progress', 'success'].map(tab => mixed.filter(item => query.tabMatches(item, tab)).length), [10, 4, 3, 1, 1, 1]);
assert.deepEqual(['ALL', 'error', 'refused', 'success'].map(query.serverStatus).map(String), ['undefined', 'error', 'error', 'success'], '失败 and 拒绝 are both the server\'s error');
assert.equal(query.refusedCount(mixed), 3);
assert.equal(query.traceTotals(mixed).failures, 7, 'the server counts refusals among failures: they are taken out where shown');
console.log('PASS: totals count failures as the server does; failures break down by model and by each provider whose attempt failed, refusals before sending apart; 拒绝 is apart from 失败 in the tabs and the breakdown');

// A request's badge: 拒绝 for a refusal, and 同类拒绝 ×N for one that stands for the same refusal again within a minute.
assert.deepEqual(plain(status.traceView({status: 'error', error_class: 'usage_limit'}, now)).label, '拒绝');
assert.equal(status.traceView({status: 'error', error_class: 'stream_incomplete'}, now).label, '失败');
assert.equal(status.traceView({status: 'in_progress', ts: now - 3600}, now).label, '可能已中断');
assert.equal(status.repeatedRefusal({repeats: 0}), null);
const repeated = status.repeatedRefusal({repeats: 4, last_seen_secs: at(2026, 9, 27, 15, 12) + 40});
assert.equal(repeated.text, '同类拒绝 ×5');assert.equal(repeated.title, '一分钟内这张卡因同一原因被拒绝了 5 次，只记这一条；最后一次 15:12:40');
assert.deepEqual(plain(status.REQUEST_REFUSALS), ['insufficient_balance', 'concurrency_limit', 'usage_limit', 'input_too_long', 'unsupported_capability', 'invalid_model', 'model_not_listed', 'model_retired'], 'billing\'s CARD_REFUSALS');
assert.equal(status.errorClassLabel('input_too_long'), '输入超过模型的上下文长度');
console.log('PASS: a refusal reads 拒绝 with 同类拒绝 ×N and when the last was; the refusal classes are billing\'s');
