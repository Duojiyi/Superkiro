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
