// Rules behind a card's support actions, without a browser: the device allowance, what 延长有效期
// makes of a card's validity (as crates/billing/src/engine.rs extend_validity computes it), what
// the card's history records, and the server's refusals in the console's words.
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
const format = load('format.ts');
const support = load('cardSupport.ts', {'./format': format, './refusal': load('refusal.ts')});
const plain = value => JSON.parse(JSON.stringify(value));
const DAY = 86400, now = 1790000000;

// 已换绑 2/5 次 · 冷却剩 5 小时: the customer's own unbindings, and the cooldown while it runs.
assert.equal(support.rebindText({rebindsUsed: 2, maxRebinds: 5, rebindCooldownUntil: now + 5 * 3600 - 10}, now), '已换绑 2/5 次 · 冷却剩 5 小时');
assert.equal(support.rebindText({rebindsUsed: 5, maxRebinds: 5, rebindCooldownUntil: now + 40 * 60}, now), '已换绑 5/5 次 · 冷却剩 40 分钟');
assert.equal(support.rebindText({rebindsUsed: 1, maxRebinds: 5, rebindCooldownUntil: now - 1}, now), '已换绑 1/5 次', 'a cooldown that is over is not shown');
assert.equal(support.rebindText({rebindsUsed: 0, maxRebinds: 5, rebindCooldownUntil: null}, now), '已换绑 0/5 次');
assert.equal(support.rebindText({}, now), null, 'older servers do not report it');
assert.equal(support.rebindsToReset({rebindsUsed: 0, rebindCooldownUntil: null}, now), false);
assert.equal(support.rebindsToReset({rebindsUsed: 0, rebindCooldownUntil: now + 60}, now), true);
assert.equal(support.rebindsToReset({rebindsUsed: 3}, now), true);
console.log('PASS: the rebind allowance reads 已换绑 N/M 次 with the cooldown left, and 重置 is offered only when it changes something');

// 延长有效期, as the server computes it.
const active = {id: 'card-a', status: 'active', activatedAt: now - 10 * DAY, validUntil: now + 2 * DAY};
assert.deepEqual(plain(support.extension(active, {days: 7}, now)), {kind: 'until', from: now + 2 * DAY, to: now + 9 * DAY}, 'from its end while it runs');
const lapsed = {...active, validUntil: now - 3 * DAY};
assert.deepEqual(plain(support.extension(lapsed, {days: 7}, now)), {kind: 'until', from: now - 3 * DAY, to: now + 7 * DAY}, 'from now once it has ended');
assert.deepEqual(plain(support.extension({...active, status: 'expired'}, {days: 1}, now)), {kind: 'until', from: now + 2 * DAY, to: now + 3 * DAY});
const waiting = {id: 'card-w', status: 'unactivated', activatedAt: null, validUntil: null, activationDurationSecs: 30 * DAY};
assert.deepEqual(plain(support.extension(waiting, {days: 7}, now)), {kind: 'duration', fromSecs: 30 * DAY, toSecs: 37 * DAY}, 'a card not yet activated keeps its validity from activation, longer');
assert.deepEqual(plain(support.extension({...waiting, activationDurationSecs: undefined}, {days: 3}, now)), {kind: 'duration', fromSecs: 30 * DAY, toSecs: 33 * DAY}, 'without one reported, the 30 days a card gets at activation');
assert.deepEqual(plain(support.extension({...waiting, status: 'frozen'}, {days: 3}, now)), {kind: 'duration', fromSecs: 30 * DAY, toSecs: 33 * DAY}, 'frozen before it was activated');
assert.deepEqual(plain(support.extension(active, {validUntilSecs: now + 20 * DAY}, now)), {kind: 'until', from: now + 2 * DAY, to: now + 20 * DAY});
for (const [card, change, reason] of [
  [{...active, status: 'voided'}, {days: 1}, '已作废'],
  [{...active, archivedAt: now - DAY}, {days: 1}, '已归档'],
  [{...active, validUntil: null}, {days: 1}, '永不过期'],
  [{...waiting, activationDurationSecs: 0}, {days: 1}, '永不过期'],
  [waiting, {validUntilSecs: now + 9 * DAY}, '未激活，只能按天数延长'],
  [active, {validUntilSecs: now + DAY}, '现在的到期时间更晚'],
]) assert.deepEqual(plain(support.extension(card, change, now)), {kind: 'refused', reason}, reason);
const end = support.endOfDay('2026-10-05');
assert.equal(new Date(end * 1000).getHours(), 23);assert.equal(new Date(end * 1000).getMinutes(), 59);assert.equal(new Date(end * 1000).getDate(), 5);
assert.equal(support.endOfDay('10/05/2026'), null);
assert.equal(support.extensionText({kind: 'duration', fromSecs: 30 * DAY, toSecs: 37 * DAY}), '激活后有效 30 天 → 37 天');
assert.equal(support.extensionText({kind: 'refused', reason: '已归档'}), '不能延期：已归档');
assert.match(support.extensionText({kind: 'until', from: now, to: now + 7 * DAY}), /^到期 \d{4}-\d{2}-\d{2} \d{2}:\d{2} → \d{4}-\d{2}-\d{2} \d{2}:\d{2}$/);
console.log('PASS: 延长有效期 adds days from the end (or from now once ended), lengthens a waiting card\'s validity, and refuses what the server refuses');

// The history: each action in words, and what it recorded besides who and why.
assert.deepEqual(['unban', 'unbind', 'rebinds_reset', 'extend', 'note', 'group', 'adjust', 'mystery'].map(support.historyLabel),
  ['解封', '解绑设备', '重置换绑次数', '延长有效期', '修改备注', '换分组', '调账', 'mystery']);
const group = id => ({g1: 'PRO', g2: 'PRO Max'})[id] ?? id;
assert.equal(support.historyDetail({action: 'unbind', detail: {deviceId: 'dev_5616aa90c3e17828'}}), '设备 dev_5616…7828');
assert.equal(support.historyDetail({action: 'rebinds_reset', detail: {previousRebinds: 2, previousCooldownUntil: null}}), '原已换绑 2 次');
assert.match(support.historyDetail({action: 'rebinds_reset', detail: {previousRebinds: 5, previousCooldownUntil: now}}), /^原已换绑 5 次，冷却到 \d{4}-\d{2}-\d{2} \d{2}:\d{2}$/);
assert.match(support.historyDetail({action: 'extend', detail: {validUntil: now}}), /^到期改为 \d{4}-\d{2}-\d{2} \d{2}:\d{2}$/);
assert.equal(support.historyDetail({action: 'extend', detail: {activationDurationSecs: 37 * DAY}}), '激活后有效 37 天');
assert.equal(support.historyDetail({action: 'group', detail: {previousGroupId: 'g1', groupId: 'g2'}}, group), 'PRO → PRO Max');
assert.equal(support.historyDetail({action: 'ban', detail: null}), '');
assert.equal(support.historyDetail({action: 'note'}), '');
console.log('PASS: history actions and their details (device, previous allowance, new expiry, groups) in words');

// Refusals in the server's words, explained; the cards they name are listed short.
for (const [text, expected] of [
  ['A reason of 1 to 200 bytes is required', '请填写原因（1–200 字节，约 60 个汉字）'],
  ['Invalid billing state: cannot unban Active', '只有已封禁的卡可以解封（这张卡现在是使用中）'],
  ['Invalid billing state: Archived cards must be unarchived before they are unbanned', '已归档的卡要先取消归档，再解封'],
  ['Device dev_5616aa7828 not found for card card-74f48022ecefbdeb', '这台设备已经不在这张卡上（可能刚被解绑或换绑）：请刷新'],
  ['Card card-74f48022ecefbdeb not found', '没有这张卡密（可能刚被删除），请刷新'],
  ['Card card-74f48022ecefbdeb, card-b802b003e19a4e48 not found', '没有这些卡密：card-74f4…bdeb、card-b802…4e48，请刷新'],
  ['Unknown group: group-vip', '分组不存在：group-vip（可能刚被删除），请刷新'],
  ['Group does not take cards: acceptance', '这个分组不接收卡密（没有开启“可发新卡”）：acceptance'],
  ['Voided cards cannot be extended: card-74f48022ecefbdeb', '已作废的卡不能延期：card-74f4…bdeb'],
  ['Archived cards must be unarchived before they are extended: card-1', '已归档的卡要先取消归档，再延期：card-1'],
  ['Cards that never expire cannot be extended: card-1, card-2', '永不过期的卡不用延期：card-1、card-2'],
  ['An expiry date applies only to activated cards: card-1', '还没激活的卡只能按天数延长（从激活起算）：card-1'],
  ['The new expiry is earlier than the current one of: card-1', '新的到期时间早于这些卡现在的到期时间：card-1'],
  ['cardIds must name 1 to 500 cards', '一次只能延长 1–500 张卡'],
  ['days must be between 1 and 3650', '延长的天数须在 1–3650 天之间'],
  ['validUntilSecs must be in the future and within 3650 days', '新的到期时间须晚于现在，且在 3650 天以内'],
  ['Give exactly one of days and validUntilSecs', '请选择延长的天数或到期日期（二选一）'],
  ['note must be at most 256 bytes, without control characters', '备注最多 256 字节（约 85 个汉字），不能包含换行等控制字符'],
  ['The change could not be saved, so nothing was changed; retry shortly', '服务器没能保存这次修改，什么都没有改：请稍后重试'],
  ['Invalid request body: unknown field `x`', '提交的内容无效，请刷新后重试'],
]) assert.equal(support.explainCardRefusal(text), expected, text);
const many = Array.from({length: 9}, (_, i) => `card-${String(i).repeat(16)}`).join(', ');
assert.equal(support.explainCardRefusal(`Voided cards cannot be extended: ${many}`), '已作废的卡不能延期：card-0000…0000、card-1111…1111、card-2222…2222、card-3333…3333、card-4444…4444、card-5555…5555 等 9 张');
assert.equal(support.explainCardRefusal('something new'), 'something new', 'unknown texts are shown as they are');
// Nothing changed: a refusal, or the server saying it could not save; anything else must be checked.
const error = (status, message) => Object.assign(new Error(message), {status});
assert.equal(support.unchanged(error(409, 'Voided cards cannot be extended: card-1')), true);
assert.equal(support.unchanged(error(404, 'Card card-1 not found')), true);
assert.equal(support.unchanged(error(503, 'The change could not be saved, so nothing was changed; retry shortly')), true);
assert.equal(support.unchanged(error(503, 'Service Unavailable')), false, 'a 503 from elsewhere is not a promise');
assert.equal(support.unchanged(error(500, 'boom')), false);
assert.equal(support.unchanged(new Error('请求超时，结果未确认；写操作请核对后重试')), false);
// Reasons and notes as the server bounds them, in bytes.
assert.equal(support.validReason('误封'), true);assert.equal(support.validReason('   '), false);
assert.equal(support.validReason('补'.repeat(66)), true);assert.equal(support.validReason('补'.repeat(67)), false, '67 CJK characters are 201 bytes');
assert.equal(support.noteProblem('淘宝 9 月'), '');assert.equal(support.noteProblem(''), '', 'empty clears the note');
assert.equal(support.noteProblem('a'.repeat(257)), '备注最多 256 字节（现在 257 字节，约 85 个汉字）');
assert.equal(support.noteProblem('第一行\n第二行'), '备注不能包含换行等控制字符');
console.log('PASS: refused support actions are explained in words with their cards listed short; only a refusal or the server\'s "could not be saved" counts as nothing changed');
