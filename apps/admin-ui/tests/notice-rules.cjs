// 公告: where one stands, the window a draft asks for, its words, an edit's fields, and the server's
// refusals in the console's words, without a browser.
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
  vm.runInNewContext(code, {exports, require: name => imports[name], Date});
  return exports;
}
const notices = load('notices.ts', {'./traceQuery': load('traceQuery.ts')});
const plain = value => JSON.parse(JSON.stringify(value));
const at = (y, m, d, h = 0, min = 0) => Math.floor(new Date(y, m - 1, d, h, min).getTime() / 1000);
const now = at(2026, 9, 27, 15, 20) + 17;

// Where one stands: the server's word first; an older server's fields otherwise.
assert.equal(notices.noticeStatus({status: 'scheduled', enabled: true}, now), 'scheduled');
assert.equal(notices.noticeStatus({enabled: false, expires_at: now + 60}, now), 'withdrawn');
assert.equal(notices.noticeStatus({enabled: true, expires_at: now - 1}, now), 'ended');
assert.equal(notices.noticeStatus({enabled: true, expires_at: null, starts_at: now + 60}, now), 'scheduled');
assert.equal(notices.noticeStatus({enabled: true, expires_at: null}, now), 'active');
console.log('PASS: where an announcement stands, by the server\'s status or, from an older server, by its fields');

// The window a draft asks for, by the server's rules.
const timing = fields => ({start: 'now', startAt: '', end: 'days', days: 7, endAt: '', ...fields});
assert.deepEqual(plain(notices.noticeWindow(timing({}), now)), {ttlSecs: 7 * 86400}, 'now, for seven days');
assert.deepEqual(plain(notices.noticeWindow(timing({start: 'at', startAt: '2026-09-28T02:00', days: 1}), now)), {startsAtSecs: at(2026, 9, 28, 2), ttlSecs: 86400});
assert.deepEqual(plain(notices.noticeWindow(timing({start: 'at', startAt: '2026-09-27T09:00'}), now)), {ttlSecs: 7 * 86400}, 'a start already past is now');
assert.deepEqual(plain(notices.noticeWindow(timing({start: 'at', startAt: '2026-09-28T02:00', end: 'at', endAt: '2026-09-28T06:00'}), now)), {startsAtSecs: at(2026, 9, 28, 2), endsAtSecs: at(2026, 9, 28, 6)});
assert.deepEqual(plain(notices.noticeWindow(timing({end: 'never'}), now)), {}, 'until withdrawn');
assert.equal(notices.noticeWindow(timing({start: 'at', startAt: '2026-09-28T02:00', end: 'at', endAt: '2026-09-28T02:00'}), now).error, '结束时间要晚于开始时间');
assert.equal(notices.noticeWindow(timing({end: 'at', endAt: '2026-09-27T15:00'}), now).error, '结束时间要晚于开始时间', 'an end already past');
assert.equal(notices.noticeWindow(timing({start: 'at', startAt: '2036-09-27T15:00'}), now).error, '开始时间要在 3650 天以内');
assert.equal(notices.noticeWindow(timing({end: 'at', endAt: '2036-09-27T15:00'}), now).error, '结束时间要在 3650 天以内');
assert.equal(notices.noticeWindow(timing({start: 'at', startAt: ''}), now).error, '请填写开始时间');
assert.equal(notices.noticeWindow(timing({end: 'at', endAt: '明天'}), now).error, '请填写结束时间');
console.log('PASS: a draft asks for a start (a past one is now), days from it, an end after it, or until withdrawn; ten years at most');

// In words.
assert.equal(notices.windowText({ttlSecs: 7 * 86400}, now), '立即开始 · 7 天后结束');
assert.equal(notices.windowText({startsAtSecs: at(2026, 9, 28, 2), endsAtSecs: at(2026, 9, 28, 6)}, now), '09-28 02:00 开始 · 09-28 06:00 结束');
assert.equal(notices.windowText({}, now), '立即开始 · 一直显示，直到撤回');
assert.equal(notices.momentText(at(2027, 1, 5, 2), now), '2027-01-05 02:00', 'another year is named');
const names = id => ({'group-pro-plus': 'PRO+', 'group-max': 'PRO Max'})[id] ?? id;
assert.equal(notices.audienceText([], names), '全部客户');
assert.equal(notices.audienceText(['group-pro-plus', 'group-max'], names), 'PRO+、PRO Max');
assert.equal(notices.reachText(['group-pro-plus'], names), '只对 PRO+ 分组的卡可见');
assert.equal(notices.reachText(undefined, names), '对全部客户可见');
assert.equal(notices.editText({operator: 'admin', at_secs: at(2026, 9, 27, 15, 2), changed: ['title', 'starts_at', 'audience']}, now), 'admin 09-27 15:02 改了标题、开始时间、对象');
assert.equal(notices.CLIENT_LEVEL.warning, '重要');
console.log('PASS: a window, an audience, an edit and the client\'s level word in words');

// An edit sends only what changed; null ends it never; the audience in any order is the same.
const notice = {id: 'ann-1', title: '维护', content: '正文', level: 'info', enabled: true, created_at: now - 600, starts_at: now - 600, expires_at: now + 3600, audience: ['group-max', 'group-pro-plus']};
assert.deepEqual(plain(notices.noticeEdit(notice, {title: '维护', content: '正文', level: 'info', audience: ['group-pro-plus', 'group-max']})), {}, 'nothing changed');
assert.deepEqual(plain(notices.noticeEdit(notice, {title: '维护（延期）', content: '正文', level: 'warning', endsAtSecs: null, audience: []})),
  {title: '维护（延期）', level: 'warning', endsAtSecs: null, audience: []});
assert.deepEqual(plain(notices.noticeEdit(notice, {title: '维护', content: '正文', level: 'info', startsAtSecs: now + 7200})), {startsAtSecs: now + 7200});
assert.deepEqual(plain(notices.noticeEdit(notice, {title: '维护', content: '正文', level: 'info', startsAtSecs: now - 600, endsAtSecs: now + 3600})), {}, 'the same times are not sent');
console.log('PASS: an edit sends only the fields that changed');

// The server's refusals and its 503s that changed nothing.
const words = notices.explainNotice;
assert.equal(words('Unknown group in audience: ghost'), '分组不存在（可能刚被删除），请刷新后重选：ghost');
assert.equal(words('A withdrawn announcement cannot be edited'), '已撤回的公告不能再编辑');
assert.equal(words('endsAtSecs must be after the start and within 3650 days'), '结束时间要晚于开始时间，并在 3650 天以内');
assert.equal(words('announcement not found'), '这条公告不存在（可能已被删除），请刷新');
for (const text of ['announcement fields are invalid', 'Give at most one of endsAtSecs and ttlSecs', 'startsAtSecs must be within 3650 days', 'audience names at most 50 groups',
  'announcement id is required', 'title must be 1 to 256 characters and content 1 to 20000', 'level must be info, warning or critical', 'ttlSecs must be between 60 and 2678400',
  'The edit could not be saved; the announcement is unchanged', 'announcement could not be saved; nothing was published', 'withdrawal could not be saved; the announcement is still shown',
  'Invalid request body: unknown field `pinned`, expected one of `id`, `title`']) assert.notEqual(words(text), text, text);
for (const text of ['The edit could not be saved; the announcement is unchanged', 'announcement could not be saved; nothing was published', 'withdrawal could not be saved; the announcement is still shown'])
  assert(notices.UNCHANGED_503.test(text), text);
assert(!notices.UNCHANGED_503.test('upstream timed out'));
console.log('PASS: each refusal and each "nothing changed" answer the server gives is put in words');
