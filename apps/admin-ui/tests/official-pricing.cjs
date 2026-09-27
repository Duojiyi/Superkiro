// 定价 by official price, without a browser: the one formula every screen uses (server-identical
// rounding), the plans a change publishes, and the fixture refusing what the server refuses.
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
const pricing = load('pricing.ts');
const change = load('priceChange.ts', {'./pricing': pricing});
const routes = load('routes.ts');
const official = load('officialPricing.ts', {'./priceChange': change, './routes': routes});
const close = (a, b, message) => assert(Math.abs(a - b) < 1e-9, `${message}: ${a} ≠ ${b}`);

// Claude Opus 5 at 计费倍率 0.24 and ¥0.03 a credit is 8 credits an official dollar: the server's own example.
const opus = [5, 25, 6.25, 0.5];
assert.deepEqual(opus.map(usd => official.creditsFor(usd, 0.24, 1, 0.03)), [40000000, 200000000, 50000000, 4000000]);
// f64 in the server's order, rounded half up: 4 × 0.35 ÷ 0.03 = 46,666,666.67 micro-credits.
assert.equal(official.creditsFor(4, 0.35, 1, 0.03), 46666667);
assert.equal(official.creditsFor(4, 0.35, 1, 0.03), Math.round(4 * 0.35 * 1 / 0.03 * 1e6));
// ¥ per official $ other than 1 scales credits and costs alike.
assert.equal(official.creditsFor(5, 0.24, 2, 0.03), 80000000);
assert.equal(official.yuanFor(5, 0.24, 2), 2.4);
close(official.costFor(2, 0.22, 1), 0.44, 'hanyue bills $2 input at 0.22');
const input = {official: opus, priceMultiplier: 0.24, costMultiplier: 0.22, basis: [2, 25, 6.25, 0.5], usdCny: 1, face: 0.03};
const version = official.officialVersion(input, {id: 'v-opus-55', rateCardId: 'fixture-rate', model: 'claude-opus-5-5', effectiveSecs: 0});
assert.deepEqual([version.fixed_input_credit_per_m, version.fixed_output_credit_per_m, version.fixed_cache_creation_credit_per_m, version.fixed_cache_read_credit_per_m], [40000000, 200000000, 50000000, 4000000]);
assert.deepEqual([version.input_price_per_m, version.output_price_per_m, version.cache_creation_price_per_m, version.cache_read_price_per_m], [2 * 0.22, 25 * 0.22, 6.25 * 0.22, 0.5 * 0.22], 'costs of what the upstream bills, in the server\'s order');
assert.deepEqual(plain(version.official), {input_usd_per_m: 5, output_usd_per_m: 25, cache_creation_usd_per_m: 6.25, cache_read_usd_per_m: 0.5, price_multiplier: 0.24, cost_multiplier: 0.22, cost_basis_usd_per_m: [2, 25, 6.25, 0.5], usd_cny: 1, credit_face_value_cny: 0.03});
assert.equal(version.margin_multiplier, 1); assert.equal(version.currency, 'CNY'); assert.equal(version.pricing_mode, 'fixed'); assert.equal(version.per_call_credit, 0);
assert.deepEqual(plain(official.officialOf(version)), plain(input), 'read back as it was made');
assert.equal(official.officialOf({pricing_mode: 'fixed'}), null);
for (const [patch, message] of [[{official: [0, 0, 0, 0]}, /不能都是 0/], [{official: [5, 25, 6.25, -1]}, /0–10,000/], [{priceMultiplier: 0}, /计费倍率/], [{costMultiplier: 100.5}, /成本倍率/], [{usdCny: 0}, /人民币/], [{basis: [1, 2, 3, 10001]}, /计费基准/]])
  assert.match(official.officialProblem({...input, ...patch}), message);
assert.equal(official.officialProblem(input), '');
console.log('PASS formula: server-identical credits (f64 order, half up), ¥ per $ other than 1, costs of the billed basis, the official block read back, bounds in words');

// A route is costed as billing costs it: its own basis, else the upstream's official price, × the route's, provider's or default 成本倍率.
const settings = official.readSettings({credit_face_value_cny: 0.03, usd_cny_rate: 7.25, default_cost_multiplier: 0.08, provider_cost_multipliers: {'hanyue-max': 0.22, 'kimera-direct': 0.06},
  official_prices: {'claude-opus-5-5': {input_usd_per_m: 4, output_usd_per_m: 20, cache_creation_usd_per_m: 5, cache_read_usd_per_m: 0.2, note: '官网', updated_at_secs: 9}, 'claude-opus-5': {input_usd_per_m: 5, output_usd_per_m: 25, cache_creation_usd_per_m: 6.25, cache_read_usd_per_m: 0.5}},
  route_costs: {'hanyue-max/claude-opus-5-5': {basis_usd_per_m: [2, 25, 6.25, 0.5]}, 'kimera-primary/claude-opus-5': {cost_multiplier: 0.1}}});
assert.equal(settings.usdCny, 1, '¥1 = $1 when unset');
const cost = (provider, target, extra = {}) => plain(official.costOfRoute({...settings, ...extra}, [], 'r', {provider_id: provider, target_model: target}, null, 0));
const billed = cost('hanyue-max', 'claude-opus-5-5');
assert.deepEqual([billed.how, billed.basis.source, billed.multiplier.source], ['official', 'route', 'provider']);
billed.perM.forEach((value, index) => close(value, [0.44, 5.5, 1.375, 0.11][index], 'route basis × provider multiplier'));
assert.deepEqual(cost('kimera-direct', 'claude-opus-5-5').perM.map(value => Math.round(value * 1e6) / 1e6), [0.24, 1.2, 0.3, 0.012], 'the official price × 0.06');
assert.equal(cost('kimera-primary', 'claude-opus-5').multiplier.source, 'route', 'a route\'s own multiplier first');
assert.equal(cost('elsewhere', 'claude-opus-5').multiplier.source, 'default');
assert.equal(cost('elsewhere', 'no-price').how, null, 'no basis and no price version: unknown');
assert.deepEqual(cost('elsewhere', 'claude-opus-5', {usdCny: 2}).perM, [0.8, 4, 1, 0.08], '¥ per $ applies to costs too');
// Without both a basis and a multiplier, the price versions cost it, as before; USD at the older rate.
const legacyVersions = [{id: 'v-legacy', rate_card_id: 'r', model: 'legacy', pricing_mode: 'fixed', effective_from_secs: 1, currency: 'USD', input_price_per_m: 1, output_price_per_m: 2, cache_creation_price_per_m: 3, cache_read_price_per_m: 4}];
const legacy = plain(official.costOfRoute(settings, legacyVersions, 'r', {provider_id: 'x', target_model: 'legacy'}, {exposed_model_id: 'legacy', target_provider_id: 'x', target_model: 'legacy'}, 10));
assert.deepEqual([legacy.how, legacy.legacySource, legacy.perM], ['legacy', 'model', [7.25, 14.5, 21.75, 29]]);
// A provider's multiplier reaches every route of it whose upstream has an official price, and no other.
const raised = {...settings, providers: {...settings.providers, 'kimera-direct': 0.08}};
assert.deepEqual(plain(official.costOfRoute(raised, [], 'r', {provider_id: 'kimera-direct', target_model: 'claude-opus-5'}, null, 0)).perM, [0.4, 2, 0.5, 0.04]);
assert.deepEqual(plain(official.costOfRoute(raised, legacyVersions, 'r', {provider_id: 'kimera-direct', target_model: 'legacy'}, {exposed_model_id: 'legacy', target_provider_id: 'kimera-direct', target_model: 'legacy'}, 10)).perM, [7.25, 14.5, 21.75, 29]);
console.log('PASS route costs: route basis, official price, route / provider / default multipliers, ¥ per $; legacy versions without both; a provider change reaches only official-costed routes');

// What customers pay and what we keep: 版本倍率 × 分组倍率 × 模型倍率, billing's single rounding, margins, the credits to start a request.
assert.deepEqual(plain(official.multipliers({margin_multiplier: 1}, {margin_multiplier: 0.8}, {credit_multiplier: 1.5})), {version: 1, group: 0.8, model: 1.5, product: 0.8 * 1.5});
assert.equal(official.chargeMicro([40000000, 200000000, 50000000, 4000000], [1000, 1000, 0, 0], 1), 240000);
assert.equal(official.chargeMicro([1, 1, 1, 1], [1, 1, 1, 1], 1), 1, 'rounded once, up');
// claude-opus-5 with a 150K context and 64K output: 150K × 50 + 64K × 200 = 20.3 credits to start.
assert.equal(official.startMicro([40000000, 200000000, 50000000, 4000000], 150000, 64000, 1), 20300000);
assert.equal(official.startMicro([40000000, 200000000, 50000000, 4000000], 150000, 64000, 0.5), 10150000);
close(official.marginOf(1, 0.25), 0.75, 'margin'); assert.equal(official.marginOf(0, 1), null);
const groups = [{id: 'std', name: '标准', rate_card_id: 'r', margin_multiplier: 1}, {id: 'power', name: 'Power', rate_card_id: 'r', margin_multiplier: 0.5}, {id: 'probe', name: '验收', rate_card_id: 'probe'}];
const mapping = {id: 'm-std', group_id: 'std', exposed_model_id: 'claude-opus-5-5', target_provider_id: 'hanyue-max', target_model: 'claude-opus-5-5', credit_multiplier: 1, fallback_chain: [{provider_id: 'kimera-direct', target_model: 'claude-opus-5-5'}]};
const powerMapping = {...mapping, id: 'm-power', group_id: 'power'};
const state = official.priceState(settings, [version], 'fixture-rate', [mapping, powerMapping], groups.map(group => ({...group, rate_card_id: 'fixture-rate'})), 10, [1000, 1000, 0, 0]);
assert.equal(state.version.id, 'v-opus-55');
state.yuanPerM.forEach((value, index) => close(value, [1.2, 6, 1.5, 0.12][index], '¥/M the customer pays'));
// hanyue at $2/$25 × 0.22 against Opus 5 sold at 0.24: on 1K/1K, ¥0.0072 revenue (¥0.0036 in Power at ×0.5) and ¥0.00594 cost.
close(state.routes[0].margin, (0.0036 - 0.00594) / 0.0036, 'the worst group decides');
assert(state.routes[0].margin < 0 && state.worst === Math.min(state.routes[0].margin, state.routes[1].margin), 'Power at half price loses on hanyue');
const alone = official.priceState(settings, [version], 'fixture-rate', [mapping], [{...groups[0], rate_card_id: 'fixture-rate'}], 10, [1000, 1000, 0, 0]);
close(alone.routes[0].margin, (7.2 - 5.94) / 7.2, 'hanyue keeps 17.5% on 1K/1K');
close(alone.routes[1].margin, (7.2 - 1.44) / 7.2, 'the backup at 0.06 of the official price');
console.log('PASS customer price and margins: effective multipliers, single rounding, the lowest margin over groups sharing a table, start credits as billing reserves them');

// A typical request: the median of each kind over the model's successful traces; 1K/1K without any.
const traces = [...[800, 1200, 5000].map((input, i) => ({exposed_model: 'm', status: 'success', input_tokens: input, output_tokens: 100 * (i + 1)})),
  {exposed_model: 'm', status: 'error', input_tokens: 99999, output_tokens: 0}, {exposed_model: 'other', status: 'success', input_tokens: 7, output_tokens: 7}];
assert.deepEqual(plain(official.sampleFor(traces, 'm')), {tokens: [1200, 200, 0, 0], count: 3});
assert.deepEqual(plain(official.sampleFor(traces, 'none')), {tokens: [1000, 1000, 0, 0], count: 0});
assert.deepEqual(plain(official.sampleFor([{exposed_model: 'c', status: 'success', input_tokens: 10000, output_tokens: 50, cache_read_tokens: 9000, cache_creation_tokens: 500}], 'c')).tokens, [500, 50, 500, 9000], 'cached input taken out when the trace says');
// One row per customer model: its entries in every group, groups in the configuration's order.
const entries = official.modelEntries([{id: 'b1', group_id: 'power', exposed_model_id: 'b', sort_order: 0}, {id: 'a2', group_id: 'std', exposed_model_id: 'a', sort_order: 1},
  {id: 'b2', group_id: 'std', exposed_model_id: 'b', sort_order: 0}, {id: 'a1', group_id: 'power', exposed_model_id: 'a', sort_order: 1}, {id: 'c1', group_id: 'probe', exposed_model_id: 'c'}], groups);
assert.deepEqual(plain(entries.map(entry => [entry.id, entry.mappings.map(row => row.id)])), [['b', ['b2', 'b1']], ['a', ['a2', 'a1']], ['c', ['c1']]]);
// Where a new price starts: the version's own block, the table's entry for the model, for its upstream, or nothing.
assert.equal(official.officialStart(settings, version, mapping).from, 'version');
assert.deepEqual(plain(official.officialStart(settings, null, mapping)), {official: [4, 20, 5, 0.2], from: 'model', name: 'claude-opus-5-5'});
assert.equal(official.officialStart(settings, null, {exposed_model_id: 'x', target_model: 'claude-opus-5'}).from, 'upstream');
assert.equal(official.officialStart(settings, null, {exposed_model_id: 'x', target_model: 'y'}), null);
// The cost a version records for its primary route names a basis only when the upstream bills another one.
assert.deepEqual(plain(official.primaryCost(settings, mapping, opus)), {costMultiplier: 0.22, basis: [2, 25, 6.25, 0.5], source: 'provider'});
assert.deepEqual(plain(official.primaryCost(settings, {...mapping, target_provider_id: 'kimera-direct', target_model: 'claude-opus-5'}, opus)), {costMultiplier: 0.06, basis: null, source: 'provider'});
console.log('PASS samples from traces, one row per customer model across groups, where a price starts, the recorded cost basis');

// A face value change reprices every official price in force or scheduled, in every table; others keep their credits.
const now = 1_900_000_000;
const at = (id, model, from, face = 0.03, extra = {}) => ({...official.officialVersion({...input, basis: null, costMultiplier: 0.08, face}, {id, rateCardId: 'fixture-rate', model, effectiveSecs: from}), ...extra});
const book = [at('a-old', 'model-a', now - 500), at('a-now', 'model-a', now - 100), at('a-later', 'model-a', now + 900), at('b-now', 'model-b', now - 50),
  {id: 'c-now', rate_card_id: 'fixture-rate', model: 'model-c', pricing_mode: 'fixed', effective_from_secs: now - 10, fixed_input_credit_per_m: 7},
  {...at('route', 'fixture-provider/model-a', now - 10), fixed_input_credit_per_m: 0, fixed_output_credit_per_m: 0, fixed_cache_creation_credit_per_m: 0, fixed_cache_read_credit_per_m: 0}];
const plan = official.faceValuePlan(book, {face: 0.05, usdCny: 1}, now);
assert.deepEqual(plain(plan.repriced.map(entry => [entry.before.id, entry.after.effective_from_secs])), [['a-now', 0], ['a-later', now + 900], ['b-now', 0], ['route', 0]], 'the superseded one is left alone');
assert.deepEqual(plain(plan.cancelled), ['a-later'], 'a scheduled one is withdrawn and added again at its own time');
assert.deepEqual(plain(plan.legacy.map(row => row.id)), ['c-now']);
assert.equal(plan.versions[0].fixed_output_credit_per_m, 120000000, '$25 × 0.24 ÷ ¥0.05');
assert.equal(plan.versions[3].fixed_input_credit_per_m, 0, 'a route cost still charges nothing');
assert(new Set([...book.map(row => row.id), ...plan.versions.map(row => row.id)]).size === book.length + plan.versions.length, 'new IDs');
assert.equal(official.faceValuePlan(book, {face: 0.03, usdCny: 1}, now).versions.length, 0, 'nothing to reprice at the same values');
console.log('PASS face value repricing: in force from now, scheduled withdrawn and re-added at their time, route costs at 0 credits, legacy prices listed as keeping their credits');

// The preview's affected set: a new price touches its model; a provider multiplier touches the models with a route on it.
const models = [{...mapping, id: 'm-opus', group_id: 'std', exposed_model_id: 'opus', target_provider_id: 'kimera-direct', target_model: 'claude-opus-5', fallback_chain: []},
  {...mapping, id: 'm-son', group_id: 'std', exposed_model_id: 'son', target_provider_id: 'hanyue-max', target_model: 'claude-opus-5-5', fallback_chain: []},
  {...mapping, id: 'm-son-power', group_id: 'power', exposed_model_id: 'son', target_provider_id: 'hanyue-max', target_model: 'claude-opus-5-5', fallback_chain: []}];
const priced = [at('opus-1', 'opus', now - 100), at('son-1', 'son', now - 100)].map(row => ({...row, rate_card_id: 'r'}));
const context = {models, groups, nowSecs: now, effectiveSecs: now + 60, sample: () => [1000, 1000, 0, 0]};
const newPrice = {...at('opus-2', 'opus', now + 60, 0.03), rate_card_id: 'r', official: {...at('x', 'x', 0).official, price_multiplier: 0.3}};
const impact = official.pricingImpact({settings, versions: priced}, {settings, versions: [...priced, {...newPrice, ...official.officialVersion({...input, basis: null, priceMultiplier: 0.3}, {id: 'opus-2', rateCardId: 'r', model: 'opus', effectiveSecs: now + 60})}]}, context);
assert.deepEqual(plain(impact.map(row => [row.model, row.rateCardId, row.mappings.map(m => m.group_id), row.changed])), [['opus', 'r', ['std'], true], ['son', 'r', ['std', 'power'], false]], 'every group charged from the table, together');
assert.equal(impact[0].after.credits[1], 250000000);
const provider = official.pricingImpact({settings, versions: priced}, {settings: {...settings, providers: {...settings.providers, 'hanyue-max': 0.3}}, versions: priced}, {...context, effectiveSecs: now});
assert.deepEqual(plain(provider.filter(row => row.changed).map(row => row.model)), ['son'], 'only the model routed through hanyue');
assert(provider[1].after.routes[0].margin < provider[1].before.routes[0].margin);
const promo = {...at('opus-4', 'opus', now + 9999), rate_card_id: 'r'};
const scheduledLater = official.pricingImpact({settings, versions: [...priced, promo]}, {settings, versions: [...priced, promo, {...at('opus-3', 'opus', now + 60), rate_card_id: 'r'}]}, context);
assert.equal(scheduledLater[0].overriddenBy.id, 'opus-4', 'a price already scheduled to take over later is named');
const ownSchedule = official.pricingImpact({settings, versions: priced}, {settings, versions: [...priced, {...at('opus-3', 'opus', now + 60), rate_card_id: 'r'}, promo]}, context);
assert.equal(ownSchedule[0].overriddenBy, null, 'one the change itself adds (a repriced schedule) is part of it');
assert.equal(official.largestChange([10, 10, 10, 10], [10, 14, 10, 10]), 40); assert.equal(official.largestChange([10, 0, 10, 10], [10, 1, 10, 10]), Infinity);
console.log('PASS preview impact: every group sharing the table in one row, a provider change touching only its routes, a later schedule named, the largest change');

// 上架 from an official price, into two groups at once, hidden first: one entry per group at its own
// place, one price per price table (in force at once where the table has none, a minute on where
// it already prices the model ID and a new price is asked for), the version recording its official block.
const listing = load('listing.ts', {'./priceChange': change, './routes': routes, './officialPricing': official});
const shelf = {rate_cards: [{id: 'r', name: 'R'}, {id: 'r2', name: 'R2'}],
  groups: [{id: 'pro', name: 'PRO', rate_card_id: 'r'}, {id: 'max', name: 'MAX', rate_card_id: 'r'}, {id: 'vip', name: 'VIP', rate_card_id: 'r2'}],
  models: [{id: 'a', group_id: 'pro', exposed_model_id: 'model-a', target_provider_id: 'p', target_model: 'model-a', sort_order: 0},
    {id: 'b', group_id: 'pro', exposed_model_id: 'model-b', target_provider_id: 'p', target_model: 'model-b', sort_order: 1},
    {id: 'c', group_id: 'max', exposed_model_id: 'model-c', target_provider_id: 'p', target_model: 'model-c', sort_order: 0}], versions: []};
const shelfContext = {config: shelf, providers: [{id: 'p', name: 'P'}], keys: [{id: 'k', provider_id: 'p', enabled: true}], nowSecs: now, effectiveSecs: now + 60};
const shelfInput = {providerId: 'p', targetModel: 'claude-opus-5-5', modelId: 'claude-opus-5-5', displayName: '', groupId: 'pro', place: {at: 'after', id: 'a'}, also: [{groupId: 'max', place: {at: 'first'}}],
  hidden: true, contextWindow: 200000, maxOutput: 64000, tools: true, vision: true, reasoning: true, rateMultiplier: '', creditMultiplier: '1', keepPrice: true,
  official: {...input, face: 0.03}, prices: {}, costs: {}, currency: 'CNY'};
const shelved = listing.buildListing(shelfInput, shelfContext);
assert.deepEqual(plain(shelved.mappings.map(row => [row.id, row.group_id, row.sort_order, row.visible])), [['p-claude-opus-5-5', 'pro', 1, false], ['p-claude-opus-5-5-2', 'max', -1, false]], 'hidden first, each group at its place');
assert.deepEqual(plain(shelved.models.map(row => row.id)), ['b', 'p-claude-opus-5-5', 'p-claude-opus-5-5-2'], 'the group it goes into the middle of is numbered again');
assert.equal(shelved.versions.length, 1, 'PRO and MAX share one price table: one price');
assert.deepEqual(plain([shelved.version.rate_card_id, shelved.version.effective_from_secs, shelved.version.fixed_output_credit_per_m, shelved.version.official.price_multiplier]), ['r', 0, 200000000, 0.24]);
const twoTables = listing.buildListing({...shelfInput, also: [{groupId: 'vip', place: {at: 'last'}}]}, {...shelfContext, config: {...shelf, versions: [{...version, id: 'v-r2', rate_card_id: 'r2', model: 'claude-opus-5-5', effective_from_secs: now - 10}]}});
assert.deepEqual(plain(twoTables.versions.map(row => row.rate_card_id)), ['r'], 'a table that already prices the model ID keeps its price');
const renewed = listing.buildListing({...shelfInput, keepPrice: false, also: [{groupId: 'vip', place: {at: 'last'}}]}, {...shelfContext, config: {...shelf, versions: [{...version, id: 'v-r2', rate_card_id: 'r2', model: 'claude-opus-5-5', effective_from_secs: now - 10}]}});
assert.deepEqual(plain(renewed.versions.map(row => [row.rate_card_id, row.effective_from_secs])), [['r', 0], ['r2', now + 60]], 'a new price there starts a minute on');
assert.throws(() => listing.buildListing({...shelfInput, also: [{groupId: 'pro', place: {at: 'last'}}]}, shelfContext), /只能选一次/);
assert.throws(() => listing.buildListing({...shelfInput, official: {...input, face: 0.03, priceMultiplier: 0}}, shelfContext), /计费倍率/);
assert.deepEqual(plain(listing.listingWarnings({targetModel: 'claude-opus-5-5', modelId: 'x', reasoning: false, contextWindow: 200000, maxOutput: 16000})), ['Opus 5.5 总是会思考，最大输出却不到 32K：思考也算输出，回答容易被截断']);
assert.equal(listing.listingWarnings({targetModel: 'gpt-6-astra', modelId: 'x', reasoning: true, contextWindow: 272000, maxOutput: 128000}).length, 0, '272K is the base context of a GPT model');
assert.match(listing.listingWarnings({targetModel: 'claude-sonnet', modelId: 'x', reasoning: false, contextWindow: 1000000, maxOutput: 8000})[0], /^上下文 1000K 超过 200K：价格没有长上下文档/);
console.log('PASS 上架 from an official price into two groups, hidden first: an entry per group at its place, one price per price table, the existing one kept or renewed a minute on, the warnings');

// The list, one row per customer model: its tables, lowest margin, start credits, filters, and the
// field-level changes confirmations show.
const sheet = load('sheetRules.ts', {'./officialPricing': official, './priceChange': change, './routes': routes});
const sheetSettings = official.readSettings({credit_face_value_cny: 0.03, usd_cny_rate: 7.25, default_cost_multiplier: 0.08, provider_cost_multipliers: {fast: 0.3},
  official_prices: {'model-x': {input_usd_per_m: 3, output_usd_per_m: 15, cache_creation_usd_per_m: 3.75, cache_read_usd_per_m: 0.3}}});
const sheetGroups = [{id: 'std', name: '标准', rate_card_id: 'r'}, {id: 'power', name: 'Power', rate_card_id: 'r'}, {id: 'vip', name: 'VIP', rate_card_id: 'r2'}];
const priced3 = official.officialVersion({official: [3, 15, 3.75, 0.3], priceMultiplier: 0.24, costMultiplier: 0.08, basis: null, usdCny: 1, face: 0.03}, {id: 'x-1', rateCardId: 'r', model: 'model-x', effectiveSecs: now - 10});
const sheetModels = [{id: 'x-std', group_id: 'std', exposed_model_id: 'model-x', target_provider_id: 'slow', target_model: 'model-x', max_output: 8000, context_window: 200000, fallback_chain: [{provider_id: 'fast', target_model: 'model-x'}]},
  {id: 'x-power', group_id: 'power', exposed_model_id: 'model-x', target_provider_id: 'slow', target_model: 'model-x', max_output: 8000, context_window: 200000, visible: false},
  {id: 'y-vip', group_id: 'vip', exposed_model_id: 'model-y', target_provider_id: 'slow', target_model: 'model-y', max_output: 8000, context_window: 200000}];
const rowsOf = official => sheet.sheetFacts(sheetModels, {groups: sheetGroups, versions: [priced3, {...priced3, id: 'x-2', effective_from_secs: now + 600}], settings: official, nowSecs: now, sample: () => [1000, 1000, 0, 0]});
const [xRow, yRow] = rowsOf(sheetSettings);
assert.deepEqual(plain([xRow.id, xRow.mappings.map(m => m.id), xRow.tables.length, xRow.tables[0].mappings.length]), ['model-x', ['x-std', 'x-power'], 1, 2], 'one row, one table for both groups');
close(xRow.worst, (24 * 1000 + 120 * 1000) / 1e6 * 0.03 > 0 ? 1 - (15 * 0.3 + 3 * 0.3) / 1000 / ((24 + 120) / 1000 * 0.03) : 0, 'the lowest margin is the backup route at ×0.3');
assert.equal(xRow.official.priceMultiplier, 0.24); assert.equal(xRow.scheduled, true); assert.equal(xRow.unofficial, false); assert.equal(xRow.noCost, false);
assert.equal(xRow.start, Math.ceil(1000 * 30 + 8000 * 120), 'input at the dearest input-side price, the full output');
assert.deepEqual(plain([yRow.unofficial, yRow.worst, yRow.start]), [true, null, null], 'a model with no price');
assert.deepEqual(plain(rowsOf({...sheetSettings, defaultCost: null}).map(entry => entry.noCost)), [true, true], 'no 成本倍率 for the slow provider');
const none = {loss: false, below: null, unofficial: false, noCost: false, scheduled: false};
assert.deepEqual(plain([xRow, yRow].filter(entry => sheet.passes(entry, {...none, unofficial: true})).map(entry => entry.id)), ['model-y']);
assert.deepEqual(plain([xRow, yRow].filter(entry => sheet.passes(entry, {...none, below: 0.99, scheduled: true})).map(entry => entry.id)), ['model-x']);
assert.deepEqual(plain([xRow, yRow].filter(entry => sheet.passes(entry, {...none, loss: true})).map(entry => entry.id)), ['model-x'], 'the backup at ×0.3 loses money');
assert.deepEqual(plain(['live', 'mixed'].map((_, index) => sheet.combinedState(index ? sheetModels.slice(0, 2) : [sheetModels[0]]))), ['live', 'mixed']);
const serves = target => target.provider_id !== 'fast';
const change_ = sheet.entryChanges(sheetModels[0], {...sheetModels[0], context_window: 300000, aliases: ['model-x-latest'], fallback_chain: [{provider_id: 'fast', target_model: 'model-x'}, {provider_id: 'fast', target_model: 'model-x-5'}]},
  {serves, provider: id => ({fast: '快线', slow: '慢线'})[id] ?? id});
assert.deepEqual(plain(change_), {lines: ['上下文 200K → 300K', '新增备用 快线 / model-x-5（不能服务）', '新增别名 model-x-latest'], unservable: ['快线 / model-x-5']});
assert.deepEqual(plain(sheet.entryChanges(sheetModels[0], {...sheetModels[0], target_provider_id: 'fast', fallback_chain: []}, {serves, provider: id => id}).lines), ['主线路 slow / model-x → fast / model-x（不能服务）'], 'a backup made the primary is not a removed backup');
console.log('PASS model sheet: one row per customer model across groups, lowest margin over every route, start credits, filters, combined state, field-level changes with routes that cannot serve');

// The fixture refuses what the server refuses, in its words (crates/billing/src/commercial.rs).
const fixture = require('./fixture-api.cjs')();
const reason = 'official pricing';
const send = body => fixture.publish({expected_revision: fixture.config.revision, reason, ...body});
const refusal = result => result.body.error?.replace('Invalid billing state: ', '');
const face = 0.01, first = official.officialVersion({...input, face}, {id: 'astra-official', rateCardId: 'fixture-rate', model: 'brand-new', effectiveSecs: 0});
assert.equal(refusal(send({versions: [{...first, fixed_output_credit_per_m: first.fixed_output_credit_per_m + 2}]})), 'Official pricing does not match the credits of: brand-new');
assert.equal(send({versions: [{...first, fixed_output_credit_per_m: first.fixed_output_credit_per_m + 1}], reason: 'within one micro-credit'}).status, 200);
assert.equal(refusal(send({versions: [{...first, id: 'x', model: 'x', output_price_per_m: first.output_price_per_m * 1.001}]})), 'Official pricing does not match the cost of: x');
assert.equal(refusal(send({versions: [{...first, id: 'y', model: 'y', margin_multiplier: 1.3}]})), 'Invalid official pricing: y');
assert.equal(refusal(send({versions: [{...first, id: 'z', model: 'z', official: {...first.official, credit_face_value_cny: 0.03}, ...official.officialVersion({...input, face: 0.03}, {id: 'z', rateCardId: 'fixture-rate', model: 'z', effectiveSecs: 0})}]})), 'Official pricing is at a stale face value or rate: z');
assert.equal(send({versions: [{...first, id: 'route-x', model: 'fixture-provider/x', ...Object.fromEntries(['fixed_input_credit_per_m', 'fixed_output_credit_per_m', 'fixed_cache_creation_credit_per_m', 'fixed_cache_read_credit_per_m'].map(field => [field, 0]))}]}).status, 200, 'a route cost charges nothing');
assert.equal(refusal(send({versions: [{...first, id: 'vendor-x', model: 'vendor/x', ...Object.fromEntries(['fixed_input_credit_per_m', 'fixed_output_credit_per_m', 'fixed_cache_creation_credit_per_m', 'fixed_cache_read_credit_per_m'].map(field => [field, 0]))}]})), 'Official pricing does not match the credits of: vendor/x', 'a "/" that names no provider is a model');
assert.equal(send({versions: [{...first, official: undefined, id: 'no-field'}].map(({currency, ...rest}) => rest)}).status, 400, 'a missing field is malformed');
// Settings: newer fields left out keep their value; bounds; the server stamps official prices.
const kept = {credit_face_value_cny: 0.01, usd_cny_rate: 7.2};
assert.equal(send({settings: {...kept, default_price_multiplier: 0.24, provider_cost_multipliers: {'fixture-provider': 0.08}, official_prices: {'claude-sonnet': {input_usd_per_m: 3, output_usd_per_m: 15, cache_creation_usd_per_m: 3.75, cache_read_usd_per_m: 0.3}}, route_costs: {'fixture-provider/claude-sonnet': {cost_multiplier: 0.1}}}}).status, 200);
assert.equal(send({settings: {...kept, usd_cny_rate: 7.3}}).status, 200, 'what the old 结算参数 sends');
assert.deepEqual([fixture.config.settings.default_price_multiplier, fixture.config.settings.provider_cost_multipliers, fixture.config.settings.usd_cny_rate], [0.24, {'fixture-provider': 0.08}, 7.3]);
assert(fixture.config.settings.official_prices['claude-sonnet'].updated_at_secs > 0, 'stamped by the server');
for (const [patch, message] of [[{official_usd_cny: 0}, 'The official dollar rate must be positive and at most 1000'], [{default_cost_multiplier: 100.5}, 'Multipliers must be positive and at most 100, for at most 200 providers'],
  [{route_costs: {'/claude': {cost_multiplier: 0.1}}}, /^Route costs/], [{route_costs: {'p/': {cost_multiplier: 0.1}}}, /^Route costs/], [{official_prices: {x: {input_usd_per_m: 10001, output_usd_per_m: 1, cache_creation_usd_per_m: 1, cache_read_usd_per_m: 1}}}, /^Official prices/]])
  assert.match(refusal(send({settings: {credit_face_value_cny: 0.01, usd_cny_rate: 7.3, ...patch}})), message instanceof RegExp ? message : new RegExp(`^${message.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}$`));
// A face value change: every official price in force or scheduled at the new value, repriced from now; a scheduled one withdrawn and re-added.
const t = Math.floor(Date.now() / 1000);
// The prices above started a while ago (a second price in the same second is a duplicate).
for (const row of fixture.config.versions) if (row.effective_from_secs >= t - 5) row.effective_from_secs = t - 100;
assert.equal(send({versions: [official.officialVersion({...input, face: 0.01}, {id: 'later', rateCardId: 'fixture-rate', model: 'brand-new', effectiveSecs: t + 900})]}).status, 200);
assert.equal(refusal(send({settings: {...kept, usd_cny_rate: 7.3, credit_face_value_cny: 0.02}})), 'Official pricing is at a stale face value or rate: brand-new, fixture-provider/x');
const reprice = official.faceValuePlan(fixture.config.versions, {face: 0.02, usdCny: 1}, t);
assert.equal(refusal(send({settings: {...kept, usd_cny_rate: 7.3, credit_face_value_cny: 0.02}, versions: reprice.versions.filter(row => row.effective_from_secs === 0)})), 'Official pricing is at a stale face value or rate: brand-new', 'the scheduled one is stale until withdrawn or repriced');
assert.equal(refusal(send({settings: {...kept, usd_cny_rate: 7.3, credit_face_value_cny: 0.02}, versions: reprice.versions})), 'Published prices immutable; use new ID and timestamp', 'repriced at its time only once the old one is withdrawn');
const done = send({settings: {...kept, usd_cny_rate: 7.3, credit_face_value_cny: 0.02}, versions: reprice.versions, cancelled_versions: reprice.cancelled});
assert.equal(done.status, 200, JSON.stringify(done.body));
assert(fixture.config.versions.some(row => row.model === 'brand-new' && row.effective_from_secs === t + 900 && row.official.credit_face_value_cny === 0.02), 'repriced at its own time');
assert.equal(refusal(send({versions: [{...official.officialVersion({...input, face: 0.02}, {id: 'back', rateCardId: 'fixture-rate', model: 'brand-new', effectiveSecs: 0})}]})), 'Invalid pricing; retroactive publication forbidden', 'outside a face value change, a priced model is never back-dated');
// Aliases stay unambiguous within a group.
const sonnet = fixture.config.models.find(row => row.id === 'fixture-model-0');
assert.equal(refusal(send({models: [{...sonnet, aliases: ['gpt-6-astra']}]})), 'Ambiguous model ID or alias');
console.log('PASS fixture contract: credits within one micro-credit, costs, invalid blocks, route costs by provider prefix, settings kept when left out and bounded, face value repricing with withdrawals, no back-dating otherwise, unambiguous aliases');
