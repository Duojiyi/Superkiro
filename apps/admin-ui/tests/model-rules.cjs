// Rules behind 模型与定价 and 供应商与 Key, without a browser: routes, refusals.
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
  vm.runInNewContext(code, {exports, require: name => imports[name], TextEncoder});
  return exports;
}
// Values made inside a module's context compare by content, not by prototype.
const plain = value => JSON.parse(JSON.stringify(value));

// A target serves when its provider is enabled and an enabled Key of it allows the upstream model.
const routes = load('routes.ts');
const providers = [{id: 'a', name: 'A'}, {id: 'b', name: 'B'}, {id: 'off', name: 'Off', enabled: false}];
const keys = [{id: 'ka', provider_id: 'a', enabled: true, allowed_models: ['m1', 'm2']}, {id: 'kb', provider_id: 'b', allowed_models: ['m1', 'm3']},
  {id: 'kb-off', provider_id: 'b', enabled: false, allowed_models: ['m4']}, {id: 'k-old', provider_id: 'off'}];
const data = {providers, keys};
const state = (provider_id, target_model) => plain(routes.targetState({provider_id, target_model}, data));
assert.deepEqual(state('a', 'm1'), {provider_id: 'a', target_model: 'm1', ok: true});
assert.equal(state('a', 'm3').problem, 'no_key');
assert.equal(state('b', 'm4').problem, 'no_key', 'a disabled Key serves nothing');
assert.equal(state('off', 'm1').problem, 'provider_disabled', 'a Key without a list does not help a disabled provider');
assert.equal(state('gone', 'm1').problem, 'no_provider');
assert.equal(routes.targetProblem(routes.targetState({provider_id: 'a', target_model: 'm9'}, data), providers), 'A 没有启用的 Key 授权 m9');
assert.equal(routes.targetProblem(routes.targetState({provider_id: 'off', target_model: 'm1'}, data), providers), 'Off 已停用');
assert.deepEqual(plain(routes.authorizedModels('b', keys)), ['m1', 'm3']);
const model = (id, provider, target, extra = {}) => ({id, exposed_model_id: id, group_id: 'g', target_provider_id: provider, target_model: target, visible: true, ...extra});
const both = model('both', 'a', 'm1', {fallback_chain: [{provider_id: 'b', target_model: 'm1'}]});
const primaryOnly = model('primary-only', 'a', 'm2');
const backupOnly = model('backup-only', 'off', 'm1', {fallback_chain: [{provider_id: 'b', target_model: 'm3'}]});
const dead = model('dead', 'b', 'm2');
const hidden = model('hidden', 'b', 'm2', {visible: false});
const retired = model('retired', 'b', 'm2', {retired: true});
assert.deepEqual(plain(routes.targetsOf(both)), [{provider_id: 'a', target_model: 'm1'}, {provider_id: 'b', target_model: 'm1'}]);
assert.equal(routes.modelRoute(backupOnly, data).down, false, 'a backup serves when the primary cannot');
assert.equal(routes.modelRoute(dead, data).down, true);
assert.deepEqual(plain(routes.brokenRoutes([both, primaryOnly, backupOnly, dead, hidden, retired], data).map(entry => [entry.model.id, entry.route.down])),
  [['backup-only', false], ['dead', true]], 'only shown, served models count; hidden and retired ones do not');
// What disabling provider a takes away: one model is left with nothing, one is served by its backup.
const models = [both, primaryOnly, backupOnly, dead, hidden];
const disabled = {providers: providers.map(item => item.id === 'a' ? {...item, enabled: false} : item), keys};
const losses = routes.routeLosses(models, data, disabled);
assert.deepEqual(plain({down: losses.down.map(row => row.id), takeover: losses.takeover.map(row => row.id), backup: losses.backup.map(row => row.id)}),
  {down: ['primary-only'], takeover: ['both'], backup: []}, 'an already dead or hidden model loses nothing');
// Deleting Key kb takes the only route of backup-only and a backup of both.
const withoutKb = {providers, keys: keys.filter(key => key.id !== 'kb')};
const keyLosses = routes.routeLosses(models, data, withoutKb);
assert.deepEqual(plain([keyLosses.down.map(row => row.id), keyLosses.takeover.map(row => row.id), keyLosses.backup.map(row => row.id)]), [['backup-only'], [], ['both']]);
assert.deepEqual(plain(routes.lossFacts(keyLosses, row => row.id)), ['将无可用线路（客户请求会失败）：backup-only', '少一条备用线路（仍可服务）：both']);
// Names: the ID, and the group only when two groups share it; long lists are cut with a count.
const groups = [{id: 'g', name: 'PRO'}, {id: 'h', name: 'Power'}];
assert.equal(routes.modelName(both, [both, {...both, id: 'other', group_id: 'h'}], groups), 'both（PRO）');
assert.equal(routes.modelName(both, [both], groups), 'both');
assert.equal(routes.nameList(['a', 'b', 'c'], 2), 'a、b 等 3 个');
console.log('PASS routes: a target needs an enabled provider and an enabled allowing Key; backups take over; losses from a provider or Key change; model names');

// Refusals: nothing changed, in words that say what to fix, naming the models; everything else is unconfirmed.
const refusal = load('refusal.ts');
const refused = (message, status = 409) => Object.assign(new Error(message), {status});
const names = id => ({'map-1': 'gpt-5（PRO）'}[id] ?? id);
assert.deepEqual(plain(refusal.publishFailure(refused('Invalid billing state: Configuration changed; reload before publishing'), '发布')), {ok: false, conflict: true, message: '配置刚被更新，请重新加载后再发布（已填的内容会保留）'});
const visible = refusal.publishFailure(refused('Invalid billing state: Visible model target has no enabled compatible key: gpt-5, gemini-pro'), '发布');
assert.equal(visible.uncertain, undefined);
assert(visible.message.startsWith('服务器拒绝了这次发布：gpt-5、gemini-pro 的主线路没有可用的 Key'), visible.message);
assert.equal(refusal.explainRefusal('Invalid billing state: Only hidden or retired mappings can be removed: map-1', names), '只能删除已隐藏或已下架的模型：gpt-5（PRO） 还在售');
assert.equal(refusal.explainRefusal('Invalid model ID: bad id'), '模型 ID 无效：bad、id（只能用字母、数字和 . _ : / -，最多 128 个字符）');
assert.match(refusal.explainRefusal('Invalid billing state: Invalid pricing; retroactive publication forbidden'), /早于现在/);
assert.match(refusal.explainRefusal('Key still serves visible models: map-1', names), /gpt-5（PRO）/);
assert.equal(refusal.explainRefusal('something new'), 'something new', 'unknown refusals are shown as they are');
assert.equal(refusal.publishFailure(refused('Invalid body', 400), '发布').uncertain, undefined, 'a malformed request was refused, not applied');
for (const error of [refused('Billing persistence failed: disk', 503), new Error('请求超时，结果未确认；写操作请核对后重试'), refused('bad gateway', 502)]) {
  assert.equal(refusal.publishFailure(error, '发布').uncertain, true, error.message);
}
console.log('PASS refusals: 409 and request errors are refusals in plain words with model names; timeouts and server errors are unconfirmed');

// 重新加载并保留修改: edits reapplied where the field is untouched on the server; the rest reported.
const {rebaseDraft, rebaseRows} = load('rebase.ts');
const base = [{id: 'a', context_window: 200000, visible: true}, {id: 'b', context_window: 100000, max_output: 8000}, {id: 'gone', context_window: 1}];
const edited = [{id: 'a', context_window: 1000000, visible: true}, {id: 'b', context_window: 128000, max_output: 16000}, {id: 'gone', context_window: 2}, {id: 'new', context_window: 5}, {id: 'taken', context_window: 6}];
const server = [{id: 'a', context_window: 200000, visible: false}, {id: 'b', context_window: 64000, max_output: 8000}, {id: 'taken', context_window: 7}];
const rebased = rebaseRows(base, edited, server);
assert.deepEqual(plain(rebased.rows), [{id: 'a', context_window: 1000000, visible: false}, {id: 'b', context_window: 64000, max_output: 16000}, {id: 'taken', context_window: 7}, {id: 'new', context_window: 5}],
  'a field changed only here is applied; one changed on the server keeps the server value; another field changed there stays');
assert.equal(rebased.applied, 3);
assert.deepEqual(plain(rebased.skipped.map(item => [item.row.id, item.reason, item.field ?? null, item.server ?? null])),
  [['b', 'changed', 'context_window', 64000], ['gone', 'removed', null, null], ['taken', 'exists', null, null]]);
const whole = rebaseDraft({models: base, versions: []}, {models: edited.slice(0, 1), versions: [{id: 'v-new'}, {id: 'v-taken'}]}, {models: server, versions: [{id: 'v-taken'}]});
assert.deepEqual(plain(whole.draft.versions), [{id: 'v-new'}], 'a new price version whose ID has since been used is dropped and reported');
assert.equal(whole.skipped.at(-1).reason, 'exists');
console.log('PASS rebase: edits reapplied onto the latest configuration where untouched, conflicts, deletions and taken IDs reported');

// 线路采购价: what a route costs, found as billing finds it; a staged cost is never a customer price.
const pricing = load('pricing.ts');
const change = load('priceChange.ts', {'./pricing': pricing});
const t = Math.floor(Date.now() / 1000);
const priced = [
  {id: 'v-model', rate_card_id: 'r', model: 'claude-x', pricing_mode: 'fixed', effective_from_secs: t - 50, currency: 'USD', input_price_per_m: 3, output_price_per_m: 15, cache_creation_price_per_m: 3.75, cache_read_price_per_m: 0.3},
  {id: 'v-route', rate_card_id: 'r', model: 'b/claude-x', pricing_mode: 'fixed', effective_from_secs: t - 10, currency: 'CNY', input_price_per_m: 1, output_price_per_m: 5, cache_creation_price_per_m: 1.25, cache_read_price_per_m: 0.1},
  {id: 'v-later', rate_card_id: 'r', model: 'c/other', pricing_mode: 'fixed', effective_from_secs: t + 600},
  {id: 'v-star', rate_card_id: 'star', model: '*', pricing_mode: 'per_call', effective_from_secs: t - 10},
];
const mapped = {id: 'm', exposed_model_id: 'claude-x', target_provider_id: 'a', target_model: 'claude-x'};
const cost = (rateCard, provider, target, row = mapped) => {const found = change.routeCost(priced, rateCard, {provider_id: provider, target_model: target}, row, t); return [found.source, found.version?.id ?? null];};
assert.deepEqual(cost('r', 'b', 'claude-x'), ['route', 'v-route'], 'a route of its own comes first');
assert.deepEqual(cost('r', 'a', 'claude-x'), ['upstream', 'v-model'], 'then the upstream model name');
assert.deepEqual(cost('r', 'c', 'other'), [null, null], 'a cost not in force yet does not count');
assert.deepEqual(cost('star', 'c', 'other'), ['wildcard', 'v-star']);
assert.deepEqual(cost('r', 'a', 'upstream-y', {...mapped, exposed_model_id: 'claude-x', target_model: 'upstream-y'}), ['model', 'v-model'], "the model's own price version, for its primary route");
assert.equal(change.costText(priced[1]), 'CNY 1 / 5 / 1.25 / 0.1');
const staged = change.buildRouteCost({costs: {input_price_per_m: '1', output_price_per_m: '5', cache_creation_price_per_m: '1.25', cache_read_price_per_m: '0.1'}, currency: 'CNY'},
  {providerId: 'b', targetModel: 'claude-x', rateCardId: 'r', nowSecs: t, taken: []});
assert.equal(staged.model, 'b/claude-x');assert.equal(staged.effective_from_secs, 0);assert.equal(staged.margin_multiplier, 1);
for (const field of ['fixed_input_credit_per_m', 'fixed_output_credit_per_m', 'fixed_cache_creation_credit_per_m', 'fixed_cache_read_credit_per_m', 'per_call_credit']) assert.equal(staged[field], 0, `${field}: never charged`);
assert.throws(() => change.buildRouteCost({costs: {}, currency: 'CNY'}, {providerId: 'b', targetModel: 'x', rateCardId: 'r', nowSecs: t, taken: []}), /采购价需在/);
assert.deepEqual(plain(change.routeCostOf(priced[1], [{id: 'b', name: 'B'}, {id: 'b2'}])), {provider: {id: 'b', name: 'B'}, target: 'claude-x'});
assert.equal(change.routeCostOf(priced[0], [{id: 'b'}]), null, 'a customer price is not a route cost');
// Marked 0: at once for a first version of that model in its table, otherwise at the later time.
assert.deepEqual(plain(change.timeDraftVersions([{...staged}, {...staged, id: 'x', model: 'new/route'}, {id: 'timed', rate_card_id: 'r', model: 'b/claude-x', effective_from_secs: t + 99}], priced, t + 120)
  .map(version => [version.id, version.effective_from_secs])), [[staged.id, t + 120], ['x', 0], ['timed', t + 99]]);
// 切换线路: the new primary, the old one kept as the first backup (or not), no duplicates, at most 8.
const switched = routes.switchedRoute({target_provider_id: 'a', target_model: 'm1', fallback_chain: [{provider_id: 'b', target_model: 'm1'}, {provider_id: 'c', target_model: 'm1'}]}, {provider_id: 'b', target_model: 'm1'}, true);
assert.deepEqual(plain(switched), {target_provider_id: 'b', target_model: 'm1', fallback_chain: [{provider_id: 'a', target_model: 'm1'}, {provider_id: 'c', target_model: 'm1'}]}, 'the new primary leaves the backups; the old one leads them');
assert.deepEqual(plain(routes.switchedRoute({target_provider_id: 'a', target_model: 'm1'}, {provider_id: 'b', target_model: 'm2'}, false).fallback_chain), []);
assert.equal(routes.switchedRoute({target_provider_id: 'a', target_model: 'm', fallback_chain: Array.from({length: 8}, (_, i) => ({provider_id: `p${i}`, target_model: 'm'}))}, {provider_id: 'z', target_model: 'm'}, true).fallback_chain.length, 8);
console.log('PASS route costs: own route first, then upstream, *, the model price; staged costs never charge; 0 made later when not first; switching keeps the old route as backup');

// List order: moving a model numbers its whole group again, so no tie remains; the default is the first shown model.
const listingRules = load('listing.ts', {'./priceChange': change, './routes': routes});
const ordered = [{id: 'a', group_id: 'g', sort_order: 0, visible: false}, {id: 'b', group_id: 'g', sort_order: 0}, {id: 'c', group_id: 'g', sort_order: 5}, {id: 'x', group_id: 'h', sort_order: 0}];
const placed = (to, id) => plain(listingRules.reorder(ordered, id, to).map(row => [row.id, row.sort_order]));
assert.deepEqual(placed('first', 'c'), [['a', 1], ['b', 2], ['c', 0], ['x', 0]], 'to the top; another group is untouched');
assert.deepEqual(placed('down', 'a'), [['a', 1], ['b', 0], ['c', 2], ['x', 0]]);
assert.deepEqual(placed('up', 'b'), [['a', 1], ['b', 0], ['c', 2], ['x', 0]], 'ties are settled in the order shown');
assert.deepEqual(placed('up', 'a'), [['a', 0], ['b', 1], ['c', 2], ['x', 0]], 'already first: only renumbered');
assert.equal(listingRules.defaultModel(ordered, 'g').id, 'b', 'a hidden model is never the default');
assert.equal(listingRules.defaultModel([{id: 'r', group_id: 'g', retired: true}], 'g'), undefined);
console.log('PASS list order: up, down and to the top renumber the group; ties settled; the default is the first model customers see');

// 批量调价: × a factor or ± a percentage, exactly, rounded half up to one micro-credit.
assert.equal(change.scaledPrice(3000000, 'factor', '1.2'), 3600000);
assert.equal(change.scaledPrice(15000000, 'percent', '-20'), 12000000);
assert.equal(change.scaledPrice(15000000, 'percent', '+5'), 15750000);
assert.equal(change.scaledPrice(3, 'percent', '−10'), 3, 'the minus sign the console shows is accepted; 2.7 rounds to 3');
assert.equal(change.scaledPrice(1, 'factor', '0.5'), 1, 'half rounds up');
assert.equal(change.scaledPrice(1250000, 'factor', '0.9'), 1125000);
for (const [how, value, pattern] of [['percent', '-100', /100%/], ['factor', '0', /大于 0/], ['factor', 'abc', /系数/], ['factor', '1e3', /系数/], ['percent', '', /百分比/]]) {
  assert.throws(() => change.scaledPrice(1000, how, value), pattern, `${how} ${value}`);
}
assert.throws(() => change.scaledPrice(change.MAX_PRICE_MICRO, 'factor', '2'), /最多 1,000,000/);
console.log('PASS bulk prices: factors and percentages exact, half up, with their limits');

// States as customers meet them, and the refusals traces now name.
const status = load('status.ts');
assert.deepEqual([{}, {visible: false}, {retired: true}, {visible: false, retired: true}].map(row => status.modelStateView(row).label), ['在售', '隐藏', '已下架', '已下架']);
assert.equal(status.errorClassLabel('model_retired'), '模型已下架');assert.equal(status.errorClassLabel('no_route'), '无可用线路');assert.equal(status.errorClassLabel('something_new'), 'something_new');
console.log('PASS model states 在售 / 隐藏 / 已下架 and refusal classes in words');
