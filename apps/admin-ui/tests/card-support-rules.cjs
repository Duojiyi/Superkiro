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
// What a note or an extension replaced, and what 解封 made of the card, when the server says.
assert.match(support.historyDetail({action: 'extend', detail: {validUntil: now + 7 * DAY, previousValidUntil: now}}), /^到期 \d{4}-\d{2}-\d{2} \d{2}:\d{2} → \d{4}-\d{2}-\d{2} \d{2}:\d{2}$/);
assert.equal(support.historyDetail({action: 'extend', detail: {activationDurationSecs: 37 * DAY, previousActivationDurationSecs: 30 * DAY}}), '激活后有效 30 天 → 37 天');
assert.equal(support.historyDetail({action: 'note', detail: {note: '老客户续费', previousNote: '淘宝 9 月'}}), '“淘宝 9 月” → “老客户续费”');
assert.equal(support.historyDetail({action: 'note', detail: {note: null, previousNote: '代理 李四 9/27 · 批次 2026-09-27T12:21:23Z ff48'}}), '“代理 李四 9/27 · 批次 2026-09-…” → 无');
assert.equal(support.historyDetail({action: 'unban', detail: {status: 'frozen'}}), '恢复为已冻结');
assert.equal(support.unbanText('card-1', 'frozen'), '已解封 card-1：已恢复为冻结，需要解冻才能使用');
assert.equal(support.unbanText('card-1', 'active'), '已解封 card-1，恢复为使用中');
assert.equal(support.unbanText('card-1', undefined), '已解封 card-1', 'an older server says nothing of it');
console.log('PASS: history actions and their details (device, previous allowance, new expiry, groups) in words');

// 限额: requests at once, and credits a UTC day and over 30 days; blank is no limit; only what changes is sent.
{
  const card = {maxConcurrency: 2, dailyCreditLimit: null, monthlyCreditLimit: 5000000000};
  assert.equal(support.limitsText(card), '同时 2 个请求 · 每日 不限 · 近 30 天 5,000 积分');
  assert.deepEqual([support.limitInput(null), support.limitInput(500000000), support.limitInput(1500000)], ['', '500', '1.5']);
  assert.deepEqual([' ', '500', '1,000.5', '0', '0.000001', '10000000', '10000000.000001', '-1', '1.2345678', 'abc'].map(text => String(support.parseLimit(text))),
    ['null', '500000000', '1000500000', '0', '1', '10000000000000', 'undefined', 'undefined', 'undefined', 'undefined']);
  const same = support.quotaChange(card, {concurrency: '2', daily: '', monthly: '5000'});
  assert.deepEqual(plain(same), {change: {}, problems: [], lines: []}, 'nothing changes');
  const raised = support.quotaChange(card, {concurrency: '4', daily: '500', monthly: ''});
  assert.deepEqual(plain(raised.change), {maxConcurrency: 4, dailyCreditLimit: 500000000, monthlyCreditLimit: null});
  assert.deepEqual(plain(raised.lines), ['同时请求 2 → 4 个', '每日 不限 → 500 积分', '近 30 天 5,000 积分 → 不限']);
  assert.deepEqual(plain(support.quotaChange(card, {concurrency: '21', daily: '0', monthly: 'x'}).problems),
    ['同时请求数须是 1–20 的整数', '近 30 天上限须是 0–10,000,000 积分（最多 6 位小数），留空为不限']);
  assert.deepEqual(plain(support.quotaChange(card, {concurrency: '2', daily: '0', monthly: '5000'}).lines), ['每日 不限 → 0 积分（这张卡将用不了积分）']);
  assert.equal(support.historyLabel('quotas'), '修改限额');
  assert.equal(support.historyDetail({action: 'quotas', detail: {previousMaxConcurrency: 2, maxConcurrency: 4, previousDailyCreditLimit: null, dailyCreditLimit: 500000000}}),
    '同时请求 2 → 4 个 · 每日 不限 → 500 积分');
  assert.equal(support.historyDetail({action: 'quotas', detail: {previousMonthlyCreditLimit: 5000000000, monthlyCreditLimit: null}}), '近 30 天 5,000 积分 → 不限');
  for (const [text, expected] of [
    ['Give maxConcurrency, dailyCreditLimit or monthlyCreditLimit', '没有要修改的限额'],
    ['maxConcurrency must be between 1 and 20', '同时请求数须在 1–20 之间'],
    ['dailyCreditLimit and monthlyCreditLimit must be null or 0-10000000000000 micro-credits', '每日和近 30 天的积分上限须在 0–10,000,000 积分之间，或不限'],
  ]) assert.equal(support.explainCardRefusal(text), expected, text);
  // 更换卡密: the history keeps fingerprints only; its refusals in words; the customer's text says what the card holds.
  assert.equal(support.historyLabel('rekey'), '更换卡密');
  assert.equal(support.historyDetail({action: 'rekey', detail: {previousCodeFingerprint: '1a2b3c4d', codeFingerprint: '5e6f7a8b'}}), '卡密指纹 1a2b3c4d → 5e6f7a8b');
  for (const [text, expected] of [
    ['Voided cards cannot be given a new code: card-1', '已作废的卡不能更换卡密'],
    ['Archived cards must be unarchived before they are given a new code: card-1', '已归档的卡要先取消归档，再更换卡密'],
    ['another card already uses this code', '新卡密恰好和另一张卡的一样（极少见），没有更换：请再换一次'],
    ['Card encryption failed', '服务器没能加密保存新卡密，没有更换：请检查服务器的主密钥'],
  ]) assert.equal(support.explainCardRefusal(text), expected, text);
  const handout = support.rekeyHandout({id: 'card-1', pointsAvailable: 884.02, validUntil: null, activationDurationSecs: 30 * DAY, plan: {name: 'PRO+'}}, 'kiro-aaaa-bbbb');
  assert.equal(handout, '卡密：kiro-aaaa-bbbb\n套餐：PRO+\n余额：884.02 积分\n有效期：激活后 30 天\n下载地址：https://kiro.rent\n原来的卡密已停用，请用新卡密重新登录。');
  assert.match(support.rekeyHandout({id: 'card-1', pointsAvailable: 10, validUntil: now, planName: 'PRO'}, 'kiro-c'), /^卡密：kiro-c\n套餐：PRO\n余额：10 积分\n有效期：到 \d{4}-\d{2}-\d{2} \d{2}:\d{2}\n/);
  console.log('PASS: 限额 in words (不限 for none), limits typed in credits to the micro-credit, only the changed ones sent, the history with the values replaced, and the quota refusals in words; 更换卡密 keeps fingerprints only, its refusals in words, and its text for the customer says what the card holds');
}

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

// 补偿这次扣费: the charge given back, a reason naming the request, and the request it makes up for.
{
  const compensation = load('compensation.ts', {'./format': format, './status': load('status.ts')});
  const at = new Date(2026, 8, 26, 15, 34).getTime() / 1000, later = at + 3600, nowMs = (at + 86400 * 2) * 1000;
  assert.equal(compensation.microToPoints(2499600), '2.4996');assert.equal(compensation.microToPoints(3000000), '3');assert.equal(compensation.microToPoints(1), '0.000001');
  const broken = {ts: at, invocation_id: 'card-1:3f1c9a2e-5b7d', exposed_model: 'claude-opus-5-5', status: 'error', error_class: 'stream_incomplete', credits_charged: 3200000};
  assert.deepEqual(plain(compensation.compensation('card-1', [broken], nowMs)),
    {cardId: 'card-1', points: '3.2', reason: '补偿 09-26 15:34 claude-opus-5-5（输出中断）', requests: 1, invocationId: 'card-1:3f1c9a2e-5b7d'});
  assert.equal(compensation.compensation('card-1', [{...broken, status: 'client_aborted', error_class: null}], nowMs).reason, '补偿 09-26 15:34 claude-opus-5-5（客户端中断）');
  assert.equal(compensation.compensation('card-1', [{...broken, status: 'success', error_class: null}], nowMs).reason, '补偿 09-26 15:34 claude-opus-5-5');
  assert.equal(compensation.compensation('card-1', [{...broken, invocation_id: 'card-1:has space'}], nowMs).invocationId, undefined, 'an ID the server would refuse is not linked');
  assert.equal(compensation.compensation('card-1', [{...broken, credits_charged: 0}], nowMs), null, 'nothing charged, nothing to give back');
  // Several: their charges added up, named in the reason, linked to none (an adjustment names one request).
  const two = compensation.compensation('card-1', [{...broken, ts: later, credits_charged: 1500000, status: 'success', error_class: null}, broken, {...broken, credits_charged: 0}], nowMs);
  assert.deepEqual(plain(two), {cardId: 'card-1', points: '4.7', reason: '补偿 2 次请求：09-26 15:34 claude-opus-5-5（输出中断）、09-26 16:34 claude-opus-5-5', requests: 2});
  const many = compensation.compensation('card-1', Array.from({length: 12}, (_, i) => ({...broken, ts: at + i * 60})), nowMs);
  assert.equal(many.reason, '补偿 12 次请求（09-26 15:34 至 09-26 15:45）', 'a long list is summed up by its times');
  assert.equal(many.points, '38.4');
  // A trace names a request by its card, a colon and the client's ID: up to 257 characters.
  assert.equal(compensation.linkableInvocation(`${'c'.repeat(128)}:${'i'.repeat(128)}`), true);
  assert.equal(compensation.linkableInvocation(`${'c'.repeat(129)}:${'i'.repeat(128)}`), false);
  console.log('PASS: 补偿这次扣费 gives back the exact charge, names the request and its failure, links one request, and sums several in the reason');

  // What the server found when it would not compensate a request (billing's check_compensation), in words.
  const iso = secs => new Date(secs * 1000).toISOString().replace(/\.\d{3}Z$/, 'Z');
  const id = 'card-1:3f1c9a2e-5b7d';
  assert.deepEqual([compensation.decimalToMicro('3.2'), compensation.decimalToMicro('12'), compensation.decimalToMicro('-0.000001'), compensation.decimalToMicro('1.5x')].map(String), ['3200000', '12000000', '-1', 'NaN']);
  assert.deepEqual(plain(compensation.compensationRefusal(`Request ${id} was not found`)), {kind: 'unknown', requests: [{invocationId: id}], count: 1});
  assert.deepEqual(plain(compensation.compensationRefusal(`Request ${id} was made by card card-2, not card-1`)), {kind: 'otherCard', requests: [{invocationId: id, cardId: 'card-2'}], count: 1});
  const repeatText = `Request ${id} was charged 3.2 credits at ${iso(at)}, and was already compensated 3.2 credits at ${iso(later)} by admin (补偿 (上游) 中断); send allowRepeat with a reason to compensate it again`;
  const repeat = compensation.compensationRefusal(repeatText);
  assert.deepEqual(plain(repeat), {kind: 'repeat', count: 1, requests: [{invocationId: id, chargedMicro: 3200000, chargedAt: at, compensatedMicro: 3200000, compensatedAt: later, operator: 'admin', reason: '补偿 (上游) 中断'}]}, 'a reason may hold parentheses');
  assert.deepEqual(plain(compensation.refusalFacts(repeat, 1, nowMs)), ['这次请求 09-26 15:34 扣了 3.2 积分', '已在 09-26 16:34 由 admin 补偿过 3.2 积分（原因：补偿 (上游) 中断）']);
  assert.equal(compensation.refusalTitle(repeat), '这次请求已经补偿过，没有入账');
  const over = compensation.compensationRefusal(`Request ${id} was charged 0.5 credits at ${iso(at)}; a compensation of 5 credits is more than that; send allowRepeat with a reason to compensate more`);
  assert.deepEqual(plain(over), {kind: 'over', count: 1, requests: [{invocationId: id, chargedMicro: 500000, chargedAt: at}], askedMicro: 5000000, chargedTotalMicro: 500000});
  assert.deepEqual(plain(compensation.refusalFacts(over, 1, nowMs)), ['这次请求 09-26 15:34 扣了 0.5 积分', '这次要补偿 5 积分，多于它扣的']);
  const unknown = compensation.compensationRefusal(`Request ${id} was not found`), otherCard = compensation.compensationRefusal(`Request ${id} was made by card card-2, not card-1`);
  assert.deepEqual([repeat, over, unknown, otherCard].map(compensation.repeatable), [true, true, false, false], 'only a repeat or more than charged can be overridden');
  assert.equal(compensation.compensationRefusal('Card card-1 not found'), null);
  // A newer server's `refusal` object is read first, whatever the words say; an unreadable one falls back to them.
  const object = {kind: 'repeat', requests: [{invocationId: id, chargedMicroCredits: 3200000, chargedAtSecs: at, compensatedMicroCredits: 1000000, compensatedAtSecs: later, operator: 'ops', reason: '补偿中断'}]};
  assert.deepEqual(plain(compensation.compensationRefusal(repeatText, object)),
    {kind: 'repeat', count: 1, requests: [{invocationId: id, chargedMicro: 3200000, chargedAt: at, compensatedMicro: 1000000, compensatedAt: later, operator: 'ops', reason: '补偿中断'}]});
  assert.deepEqual(plain(compensation.compensationRefusal(repeatText, {kind: 'repeat', requests: [{chargedMicroCredits: 1}]})), plain(repeat), 'a request without its ID is not a refusal object');
  assert.deepEqual(plain(compensation.compensationRefusal(repeatText, {kind: 'mystery', requests: []})), plain(repeat));
  assert.deepEqual(plain(compensation.compensationRefusal('', {kind: 'over', requests: [{invocationId: id, chargedMicroCredits: 500000, chargedAtSecs: at}], askedMicroCredits: 5000000, chargedTotalMicroCredits: 500000})), plain(over));
  // Several requests in one adjustment: every one the server names, or, in words, how many.
  const ids = ['card-1:inv-a', 'card-1:inv-b', 'card-1:inv-c'];
  const several = compensation.compensationRefusal('', {kind: 'repeat', requests: ids.slice(0, 2).map((invocationId, i) => ({invocationId, chargedMicroCredits: 1500000, chargedAtSecs: at + i * 60, compensatedMicroCredits: 1500000, compensatedAtSecs: later, operator: 'admin', reason: '补偿 2 次请求'}))});
  assert.equal(compensation.refusalTitle(several, 3), '其中 2 次请求已经补偿过，没有入账');
  assert.deepEqual(plain(compensation.refusalFacts(several, 3, nowMs)), ['其中 2 次已经补偿过',
    '09-26 15:34 扣了 1.5 积分，已在 09-26 16:34 由 admin 补偿过 1.5 积分（原因：补偿 2 次请求）', '09-26 15:35 扣了 1.5 积分，已在 09-26 16:34 由 admin 补偿过 1.5 积分（原因：补偿 2 次请求）']);
  const counted = compensation.compensationRefusal('2 of the requests were already compensated; send allowRepeat with a reason to compensate them again');
  assert.deepEqual(plain(counted), {kind: 'repeat', requests: [], count: 2});
  assert.deepEqual(plain(compensation.refusalFacts(counted, 3, nowMs)), ['其中 2 次已经补偿过，见这张卡的操作记录']);
  const overSeveral = compensation.compensationRefusal('A compensation of 9 credits is more than the 4.5 credits these 3 requests were charged; send allowRepeat with a reason to compensate more');
  assert.deepEqual(plain(overSeveral), {kind: 'over', requests: [], count: 3, askedMicro: 9000000, chargedTotalMicro: 4500000});
  assert.equal(compensation.refusalTitle(overSeveral, 3), '补偿多于这些请求扣的积分，没有入账');
  assert.deepEqual(plain(compensation.refusalFacts(overSeveral, 3, nowMs)), ['这 3 次请求共扣了 4.5 积分', '这次要补偿 9 积分，多于它们扣的']);
  const lost = compensation.compensationRefusal('', {kind: 'unknown', requests: Array.from({length: 7}, (_, i) => ({invocationId: `card-1:inv-${i}`}))});
  assert.equal(compensation.refusalTitle(lost, 9), '服务器找不到其中 7 次请求，没有入账');
  assert.deepEqual(plain(compensation.refusalFacts(lost, 9, nowMs)), ['7 次请求在账本、归档和调用追踪里都找不到（可能已过保留期）：card-1:inv-0、card-1:inv-1、card-1:inv-2、card-1:inv-3、card-1:inv-4', '另有 2 次，见这张卡的操作记录']);
  // 仍要补偿 keeps the adjustment's reason and adds why, within what the server takes.
  assert.equal(compensation.repeatReason('补偿 09-26 15:34 gpt-5（输出中断）', ' 上次补偿后又失败了 '), '补偿 09-26 15:34 gpt-5（输出中断）；仍要补偿：上次补偿后又失败了');
  assert.equal(compensation.repeatReason('补偿', ' '), null);
  assert.equal(compensation.repeatReason('补'.repeat(490), '上次补偿不足'), null, 'longer than 500 characters together');
  console.log('PASS: a compensation the server refuses is read with what it found (its refusal object first, else its words; one request or several); only a repeat or an over-charge is offered 仍要补偿, its reason kept with why');
}

// Requests the card's own balance or limits refused, in words.
{
  const status = load('status.ts');
  assert.equal(status.traceFailureText({error_class: 'insufficient_balance', needed_micro_credits: 20300000, available_micro_credits: 15000000}), '余额不足：需要 20.3 积分，余额 15 积分');
  assert.equal(status.traceFailureText({error_class: 'insufficient_balance', needed_micro_credits: 20000000}), '余额不足：需要 20 积分');
  assert.equal(status.traceFailureText({error_class: 'insufficient_balance'}), '余额不足', 'older servers do not say what it needed');
  assert.equal(status.traceFailureText({error_class: 'concurrency_limit'}), '超过并发上限');
  assert.equal(status.traceFailureText({error_class: 'usage_limit'}), '超过每日或每月用量上限');
  assert.equal(status.traceFailureText({error_class: 'stream_incomplete'}), '输出中断');
  assert.deepEqual([...status.CARD_LIMIT_REFUSALS], ['insufficient_balance', 'concurrency_limit', 'usage_limit'], 'the classes billing names CARD_LIMIT_REFUSALS');
  console.log('PASS: balance, concurrency and usage refusals in words, a balance refusal with what it needed and what the card had');
}

// Health by attempt: a primary failing over to its backup shows its failures, as billing counts them.
{
  const health = load('health.ts', {'./format': format, './status': load('status.ts')});
  const now = 1790000000;
  const traces = [
    {ts: now - 60, attempt_chain: [{provider_id: 'hanyue', key_id: 'hanyue-1', success: false, error: 'http_529'}, {provider_id: 'kimera', key_id: 'kimera-1', success: true}]},
    {ts: now - 120, attempt_chain: [{provider_id: 'hanyue', key_id: 'hanyue-1', success: false, error: 'http_529'}, {provider_id: 'hanyue', key_id: 'hanyue-2', success: false, error: 'timeout'}]},
    {ts: now - 180, attempt_chain: [{provider_id: 'kimera', key_id: 'kimera-1', success: true}]},
    {ts: now - 90000, attempt_chain: [{provider_id: 'hanyue', key_id: 'hanyue-1', success: false, error: 'http_529'}]},
  ];
  // The upstream refused a prompt too long: it did right, and that attempt is counted apart.
  traces.push({ts: now - 240, status: 'error', error_class: 'input_too_long', attempt_chain: [{provider_id: 'kimera', key_id: 'kimera-1', success: false, error: 'http_400'}]});
  const {providers, keys} = health.attemptsFromTraces(traces, now);
  assert.deepEqual(plain(providers.get('hanyue')), {attempts: 3, failures: 3, takenOver: 1, failuresByKind: {http_529: 2, timeout: 1}, refused: 0}, 'taken over only when another provider answered later');
  assert.deepEqual(plain(providers.get('kimera')), {attempts: 2, failures: 0, takenOver: 0, failuresByKind: {}, refused: 1});
  assert.deepEqual(plain(keys.get('hanyue-1')), {attempts: 2, failures: 2, takenOver: 1, failuresByKind: {http_529: 2}, refused: 0}, 'older than 24 hours is left out');
  assert.equal(health.kindsText({http_529: 5, timeout: 2}), 'HTTP 529 · 上游过载 × 5、超时 × 2');
  assert.equal(health.kindsText({http_529: 5, timeout: 2, transport: 1}), 'HTTP 529 · 上游过载 × 5、超时 × 2 等 3 种');
  assert.equal(health.kindsText({stream_incomplete: 2}), '输出中断 × 2', 'a request\'s error class in words');
  assert.equal(health.kindsText({'fixture upstream timeout': 1}), 'fixture upstream timeout × 1', 'older free text as it is');
  assert.deepEqual([health.failureTone(1, 100), health.failureTone(5, 100), health.failureTone(10, 100), health.failureTone(0, 0)], [undefined, 'warning', 'danger', undefined]);
  assert.equal(health.failureText(7, 120), '7（5.8%）');assert.equal(health.failureText(0, 0), '—');
  const window = (requests, failures, top = null) => ({requests, failures, lastFailureAt: failures ? now : null, topFailureKind: top, failuresByKind: top ? {[top]: failures} : {}});
  assert.deepEqual(plain(health.failingModels([
    {model: 'claude-sonnet-4-6', last1h: window(40, 2, 'http_429'), last24h: window(400, 9, 'http_429'), last7d: window(900, 9)},
    {model: 'claude-opus-5-5', last1h: window(7, 7, 'upstream_start_failed'), last24h: window(20, 20, 'upstream_start_failed'), last7d: window(90, 20)},
    {model: 'claude-opus-4-8', last1h: window(0, 0), last24h: window(3, 3, 'http_401'), last7d: window(30, 3)},
    {model: 'claude-opus-4-6', last1h: window(0, 0), last24h: window(2, 2, 'http_401'), last7d: window(2, 2)},
    {model: 'gpt-5.6-sol', last1h: window(10, 0), last24h: window(50, 1), last7d: window(80, 1)},
  ])), [
    {model: 'claude-opus-5-5', window: 'hour', text: 'claude-opus-5-5 近 1 小时 7 次失败（全部失败）：上游未响应（未开始输出）'},
    {model: 'claude-opus-4-8', window: 'day', text: 'claude-opus-4-8 近 24 小时 3 次请求全部失败：HTTP 401 · Key 无效或被拒绝'},
    {model: 'claude-sonnet-4-6', window: 'hour', text: 'claude-sonnet-4-6 近 1 小时 2 次失败（共 40 次）：HTTP 429 · 限流'},
  ], 'failing in the last hour, or every request of the last 24 hours (three or more); busiest failures first');
  console.log('PASS: attempts by provider and Key with takeovers, failures by kind in words, failure tones, and the models 需要关注 names');
}
