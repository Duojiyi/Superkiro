// 套餐: the catalog's seed and order, each field's bounds as the server holds them, what the customer
// sees, a change in words, and the server's plan refusals in the console's words, without a browser.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require('typescript');
function load(file) {
  const exports = {};
  const code = ts.transpileModule(fs.readFileSync(path.join(__dirname, '../src', file), 'utf8'), {
    compilerOptions: {module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020},
  }).outputText;
  vm.runInNewContext(code, {exports, TextEncoder});
  return exports;
}
const plans = load('plans.ts'), refusal = load('refusal.ts');
const plain = value => JSON.parse(JSON.stringify(value));

// A server that stores no plans issues the four tiers: 30 days, one device, two at once, into group-pro-plus or the first group by ID.
const seed = plain(plans.seedPlans(['vip', 'group-pro-plus', 'basic']));
assert.deepEqual(seed.map(plan => [plan.id, plan.name, plan.points, plan.price_cny, plan.kiro_plan_type, plan.sort_order]),
  [['tier-1000', 'PRO', 1000, 30, 'PRO', 10], ['tier-2000', 'PRO+', 2000, 55, 'PRO_PLUS', 20], ['tier-5000', 'PRO Max', 5000, 130, 'PRO_MAX', 30], ['tier-10000', 'Power', 10000, 250, 'POWER', 40]]);
assert(seed.every(plan => plan.validity_days === 30 && plan.max_devices === 1 && plan.concurrency === 2 && plan.on_sale && plan.default_group_id === 'group-pro-plus'));
assert.equal(plans.seedPlans(['vip', 'basic'])[0].default_group_id, 'basic', 'without group-pro-plus, the first group by ID');
// The catalog in force: by sort order, then ID.
const trial = {id: 'trial-7d', name: '体验卡', points: 300, price_cny: 9.9, validity_days: 7, max_devices: 1, concurrency: 1, default_group_id: 'basic', kiro_plan_type: 'CUSTOM', on_sale: true, sort_order: 20};
const family = {...trial, id: 'family', name: '家庭', max_devices: 3, sort_order: 20};
assert.deepEqual(plain(plans.planCatalog([trial, seed[0], family], []).map(plan => plan.id)), ['tier-1000', 'family', 'trial-7d']);
assert.deepEqual(plain(plans.planCatalog(undefined, ['basic']).map(plan => plan.id)), ['tier-1000', 'tier-2000', 'tier-5000', 'tier-10000']);
assert(plans.issuable(trial) && !plans.issuable(family) && !plans.issuable({...trial, on_sale: false}), 'on sale and one device');
assert.equal(plans.priceMicro(trial), 9900000);
console.log('PASS: the four tiers when the server keeps no plans, the catalog by sort order then ID, issuable only on sale and for one device');

// Each field's bounds, as billing's Plan::problem.
const draft = fields => ({...plans.draftOf(trial), ...fields});
const ok = plain(plans.parsePlan(draft({name: ' 体验卡 ', price: '9.90'}), {groupIds: ['basic']}));
assert.deepEqual(ok.plan, trial, 'the fields become the plan, the name trimmed');
const errorOf = (fields, context = {groupIds: ['basic']}) => plain(plans.parsePlan(draft(fields), context)).errors ?? {};
for (const [fields, field] of [[{id: 'Trial'}, 'id'], [{id: 'trial_7d'}, 'id'], [{id: 't'.repeat(65)}, 'id'], [{id: ''}, 'id'],
  [{name: ' '}, 'name'], [{name: '体验卡'.repeat(4)}, 'name'], [{name: 'line\nbreak'}, 'name'],
  [{points: '0'}, 'points'], [{points: '10000001'}, 'points'], [{points: '1.5'}, 'points'],
  [{price: '-0.01'}, 'price'], [{price: '100000.01'}, 'price'], [{price: '9.999'}, 'price'], [{price: ''}, 'price'],
  [{validityDays: '0'}, 'validityDays'], [{validityDays: '3651'}, 'validityDays'], [{maxDevices: '0'}, 'maxDevices'], [{maxDevices: '2'}, 'maxDevices'], [{maxDevices: '11'}, 'maxDevices'],
  [{concurrency: '0'}, 'concurrency'], [{concurrency: '21'}, 'concurrency'], [{defaultGroupId: 'ghost'}, 'defaultGroupId'], [{kiroPlanType: 'PRO_ULTRA'}, 'kiroPlanType'],
  [{sortOrder: '1.5'}, 'sortOrder'], [{sortOrder: '2147483648'}, 'sortOrder']])
  assert(errorOf(fields)[field], `${JSON.stringify(fields)} is refused on ${field}`);
assert.equal(errorOf({}, {groupIds: ['basic'], takenIds: ['trial-7d']}).id, '已经有这个 ID 的套餐');
// At the bounds.
assert.deepEqual(errorOf({id: 'f'.repeat(64), name: 'x'.repeat(32), points: '10000000', price: '100000', validityDays: '3650', maxDevices: '1', concurrency: '20', sortOrder: '-5'}), {});
assert.equal(errorOf({maxDevices: '3'}).maxDevices, '每张卡只绑定 1 台设备：设备数只能是 1', 'a plan is for one device, as a card binds one');
assert.deepEqual(errorOf({points: '1', price: '0', validityDays: '1'}), {});
console.log('PASS: every field is held to the server\'s bounds (ID, 32-byte name, points, price to the fen, days, devices, concurrency, group, Kiro type, order), a taken ID too');

// What the customer sees, and a change in words.
assert.equal(plans.customerView(trial), '套餐「体验卡」· 自定义档位 · 300 积分 · 激活后 7 天有效 · 同时最多 1 个请求');
assert.equal(plans.kiroLabel('PRO_PLUS'), 'Kiro Pro+');
assert.deepEqual(plain(plans.planChanges(trial, {...trial, price_cny: 12, on_sale: false})), ['售价：¥9.90 → ¥12.00', '在售：在售 → 已下架']);
assert.deepEqual(plain(plans.planChanges(trial, {...trial})), [], 'nothing changed');
assert.equal(plans.planChanges(undefined, trial, id => (id === 'basic' ? '基础' : id))[1], '售价 ¥9.90 · 每张 1 台设备 · 默认分组 基础');
console.log('PASS: what the customer sees (plan name, Kiro label, points, days, concurrency) and a change in words');

// The server's plan refusals, and issuing from a plan, in words.
const words = text => refusal.explainRefusal(text);
assert.equal(words('Invalid billing state: Plan prices must be 0-100000 yuan, to the fen: trial-7d'), '套餐售价要在 0–100,000 元之间，精确到分：trial-7d');
assert.equal(words('Invalid billing state: Plans cards were issued from can only be taken off sale: tier-2000'), '已经发过卡的套餐不能删除，只能下架：tier-2000');
assert.equal(words('Invalid billing state: Unknown default group of plan: trial-7d'), '套餐的默认分组不存在（可能刚被改动），请刷新后重选：trial-7d');
assert.equal(words('Invalid billing state: Unknown plan: ghost'), '这个套餐不存在（可能刚被删除），请刷新：ghost');
assert.equal(words('Invalid billing state: At most 100 plans'), '最多只能有 100 个套餐');
assert.equal(words('plan is not on sale'), '这个套餐已下架，不能再发卡：在“套餐”里重新上架，或换一个套餐');
assert.equal(words('cards have one device; issue from a plan with max_devices 1'), '每张卡只能绑定 1 台设备：请换一个设备数为 1 的套餐');
assert.equal(words('unknown plan'), '这个套餐不存在（可能刚被删除），请刷新');
assert.equal(words('Invalid billing state: Plans allow exactly 1 device, as cards bind one: family-3'), '每张卡只绑定 1 台设备，套餐的设备数只能是 1：family-3');
for (const text of ['Plan IDs are 1-64 of a-z, 0-9 and -', 'Plan names are 1-32 bytes', 'Plan points must be 1-10000000', 'Plan validity must be 1-3650 days', 'Plans allow 1-10 devices',
  'Plan concurrency must be 1-20', 'Plan Kiro types are PRO, PRO_PLUS, PRO_MAX, POWER or CUSTOM', 'Duplicate plan', 'planId and templateId name different plans',
  "issuance requires an enabled group, the plan's credits, and maxDevices=1"]) assert.notEqual(words(`Invalid billing state: ${text}: x`), `Invalid billing state: ${text}: x`, text);
console.log('PASS: each plan refusal and each issuance refusal the server gives is put in words');
