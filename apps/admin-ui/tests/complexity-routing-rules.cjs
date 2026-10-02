// Pure rules: no network, credentials, or browser required.
const assert = require('node:assert/strict'), fs = require('node:fs'), path = require('node:path'), vm = require('node:vm'), ts = require('typescript');
function load(file, imports = {}) {
  const exports = {};
  vm.runInNewContext(ts.transpileModule(fs.readFileSync(path.join(__dirname, '../src', file), 'utf8'), {compilerOptions: {module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020}}).outputText, {exports, require: name => imports[name] ?? {}});
  return exports;
}
const rules = load('complexityRouting.ts', {'./routes':load('routes.ts')});
const plain = value => JSON.parse(JSON.stringify(value));
const providers = [{id: 'p1'}, {id: 'p2'}], models = [{id: 'm1', target_provider_id:'p1', fallback_chain:[{provider_id:'p2',target_model:'answer'}]}, {id: 'm2', target_provider_id:'p2', fallback_chain:[{provider_id:'p1',target_model:'answer'}]}];
const classifier = {provider_id: 'p1', model: 'classifier', timeout_ms: 1500, max_input_chars: 4096, daily_request_limit: 1000,
  daily_budget_micro_cny: 100001, input_price_micro_cny_per_million: 290000, output_price_micro_cny_per_million: 1234567};
const policy = {model_map_id: 'm1', mode: 'off', simple_provider_ids: ['p1'], complex_provider_ids: ['p2', 'p1']};
const config = {revision: 'r1', classifier, policies: [policy], audit: []};
const valid = () => rules.routingDraft(config);
for (const [yuan, micro] of [['0', 0], ['0.000001', 1], ['0.29', 290000], ['1.234567', 1234567], [' 00.100001 ', 100001], ['9007199254.740991', Number.MAX_SAFE_INTEGER]]) {
  assert.equal(rules.yuanToMicro(yuan), micro);
  assert.equal(rules.yuanToMicro(rules.microToYuan(micro)), micro);
}
for (const text of ['', ' ', '-1', '+1', '1e3', 'Infinity', 'NaN', '0.0000001', '0.1234567', '1,000', '.5', '1.', '9007199254.740992', '９']) assert.equal(rules.yuanToMicro(text), null, text);
for (const bad of [-1, 1.1, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1]) assert.equal(rules.microToYuan(bad), '—');
assert.deepEqual(plain(rules.newClassifier()), {provider_id: '', model: '', timeout_ms: '1500', max_input_chars: '4096', daily_request_limit: '1000', daily_budget_yuan: '', input_price_yuan: '', output_price_yuan: ''});
assert.equal(rules.routingError(valid(), models, providers), '');
for (const mutate of [d => d.classifier.provider_id = 'missing', d => d.classifier.model = ' ', d => d.classifier.model = 'a\nb',
  ...['timeout_ms', 'max_input_chars', 'daily_request_limit'].flatMap(key => ['0', '-1', '1.5', '', 'NaN', '1e3', '9007199254740992'].map(value => d => d.classifier[key] = value)),
  ...['daily_budget_yuan', 'input_price_yuan', 'output_price_yuan'].map(key => d => d.classifier[key] = ''), d => d.classifier.daily_budget_yuan = '0',
  d => d.policies[0].simple_provider_ids = [], d => d.policies[0].complex_provider_ids = [], d => d.policies[0].complex_provider_ids = ['p1', 'p1'],
  d => d.policies[0].simple_provider_ids = ['missing'], d => d.policies[0].model_map_id = 'missing', d => d.policies.push({...d.policies[0]}),
  d => d.policies[0].mode = 'invalid', d => {d.classifier = null; d.policies[0].mode = 'observe';}]) {
  const draft = valid(); mutate(draft); assert.ok(rules.routingError(draft, models, providers));
}
const draft = valid(); draft.policies.push({...policy, model_map_id: 'm2', mode: 'observe'});
assert.equal(rules.routingError(draft, models, providers), '');
assert.equal(rules.routingError({classifier: null, policies: []}, [], []), '');
assert.deepEqual(plain(rules.routingUpdate(valid(), 'r1', '  reason  ')), {expected_revision: 'r1', reason: 'reason', classifier, policies: [policy]});
assert.equal(config.policies[0].mode, 'off', 'save does not enable a mode');
const enabled = {...config, policies: [{...policy, mode: 'enforce'}]};
const stopped = rules.shutdownUpdate(enabled);
assert.equal(stopped.policies[0].mode, 'off'); assert.equal(enabled.policies[0].mode, 'enforce');
assert.deepEqual(plain(stopped.classifier), classifier); assert.equal(stopped.expected_revision, 'r1');
const ids = ['p1', 'p2'];
assert.deepEqual(plain(rules.moveProvider(ids, 1, -1)), ['p2', 'p1']);
assert.deepEqual(plain(rules.moveProvider(ids, 0, -1)), ids); assert.deepEqual(plain(rules.moveProvider(ids, 0, 1)), ['p2', 'p1']);
assert.deepEqual(ids, ['p1', 'p2']);
assert.equal(rules.budgetDay(0), '尚未开始'); assert.equal(rules.budgetDay(1), '1970-01-02（UTC）'); assert.equal(rules.budgetDay(Infinity), '日期不可用');
for (const reason of ['decision_capacity_exhausted', 'no_eligible_route', 'simple_route_unavailable', 'continuation_route_unavailable', 'default_route', 'classifier_unconfigured', 'disabled', 'capability_required', 'continuation_without_state', 'task_continuation', 'insufficient_context', 'empty_task', 'classifier_circuit_open', 'classifier_unavailable', 'classifier_busy', 'classification_pending', 'budget_exhausted', 'semantic_simple', 'semantic_complex', 'semantic_uncertain', 'classifier_timeout', 'classifier_transport_error', 'classifier_invalid_input', 'classifier_invalid_response', 'classifier_http_error']) {
  assert(!rules.decisionReason(reason).includes('未识别'), reason); assert(/[\u4e00-\u9fff]/.test(rules.decisionReason(reason)));
}
assert.match(rules.decisionReason('insufficient_context'), /不额外截断/); assert.match(rules.decisionReason('future_reason'), /人工核查/);
const decision = {invocation_id:'i1', scope:'request', request_hash:'hash', revision:'r1', model_map_id:'m1', mode:'observe', complexity:'simple', reason:'semantic_simple', provider_ids:['p2'], served_provider_id:'p1', created_at_secs:1700000000, pending:false, classifier_attempted:true, classifier_latency_ms:15, input_tokens:20, output_tokens:2, classifier_cost_micro_cny:1, usage_estimated:true, preview:false};
assert.deepEqual(plain(rules.decisionDistribution([decision, {...decision, complexity:'complex'}, {...decision, complexity:'unknown'}, {...decision, preview:true}, {...decision, pending:true}, {...decision, classifier_attempted:false}])), {simple:1, complex:1, unknown:1});
assert(rules.validRoutingConfig(config)); assert(rules.validRoutingConfig({...config, classifier:null}));
for (const bad of [null, {}, {...config, revision:''}, {...config, classifier:undefined}, {...config, classifier:{...classifier, daily_budget_micro_cny:1.5}}, {...config, audit:null}, {...config, audit:[{}]}, {...config, policies:[{...policy, simple_provider_ids:[null]}]}]) assert(!rules.validRoutingConfig(bad));
assert(rules.validDecision(decision)); assert(rules.validDecision({...decision, served_provider_id:null}));
for (const bad of [undefined, 1, '', [], {}]) assert(!rules.validDecision({...decision, served_provider_id:bad})); assert(!rules.validDecision({...decision, classifier_cost_micro_cny:-1})); assert(!rules.validDecision({...decision, mode:'bad'}));
assert(rules.validRoutingStatus({budget:{day:0,calls:0,cost_micro_cny:0},recent_decisions:[decision]}));
assert(!rules.validRoutingStatus({budget:{day:0,calls:0,cost_micro_cny:0},recent_decisions:[{}]}));
assert(!rules.validRoutingStatus({budget:null,recent_decisions:[]}));
for (const count of [0,16000,20000]) assert(rules.validRoutingStatus({budget:{day:0,calls:0,cost_micro_cny:0},retained_decisions:count,recent_decisions:[]}));
for (const count of [-1,1.5,'20000',null]) assert(!rules.validRoutingStatus({budget:{day:0,calls:0,cost_micro_cny:0},retained_decisions:count,recent_decisions:[]}));
assert(rules.validPreview({success:true,decision:{...decision,preview:true},eligible_provider_ids:['p1'],applied_provider_ids:['p1']}));
assert(!rules.validPreview({success:true,decision,eligible_provider_ids:['p1'],applied_provider_ids:['p1']}));
console.log('PASS smart routing: exact micro-CNY, limits, saved references, independent/off policies, chain ordering, shutdown immutability, UTC day, all final reason translations, non-accuracy distribution and malformed responses');

for (const [key, low, high] of [['timeout_ms',200,5000], ['max_input_chars',256,16000], ['daily_request_limit',1,100000]]) {
  for (const value of [low, high]) {const d=valid();d.classifier[key]=String(value);assert.equal(rules.routingError(d,models,providers),'',key+' boundary '+value);}
  for (const value of [low-1, high+1]) {const d=valid();d.classifier[key]=String(value);assert.ok(rules.routingError(d,models,providers),key+' outside '+value);}
}
for (const key of ['daily_budget_yuan','input_price_yuan','output_price_yuan']) {
  for (const value of ['0.000001','1000000']) {const d=valid();d.classifier[key]=value;assert.equal(rules.routingError(d,models,providers),'');}
  for (const value of ['0','1000000.000001']) {const d=valid();d.classifier[key]=value;assert.match(rules.routingError(d,models,providers),/0.000001–1000000/);}
}
const extraProviders=[...providers,{id:'outside'},{id:'disabled',enabled:false}];
assert.deepEqual(plain(rules.routingProviders(models[0],extraProviders)).map(p=>p.id),['p1','p2']);
assert.deepEqual(plain(rules.routingProviders(undefined,extraProviders)),[]);
const outside=valid();outside.policies[0].simple_provider_ids=['outside'];assert.match(rules.routingError(outside,models,extraProviders),/完整目标链/);
assert.match(rules.routingError(valid(),models,providers.map(p=>({...p,enabled:false}))),/已启用/);
assert.match(rules.routingError(valid(),models.map(m=>({...m,retired:true})),providers),/下架/);
console.log('PASS backend bounds: inclusive 200..5000ms / 256..16000 chars / 1..100000 calls / positive 1..1e12 micro-CNY; provider options and validation restricted to model primary+fallback targets');
