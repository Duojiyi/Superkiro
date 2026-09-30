// The periods 财务对账 reports on, in local time, and the ledger CSV cut to one, without a browser.
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
  vm.runInNewContext(code, {exports, Date});
  return exports;
}
const period = load('period.ts');
const at = (y, m, d, h = 0) => Math.floor(new Date(y, m - 1, d, h).getTime() / 1000);
const plain = value => JSON.parse(JSON.stringify(value));
const now = new Date(2026, 8, 27, 15, 20);
// From the start of the first day up to, not including, the start of the day after the last.
assert.deepEqual(plain(period.periodRange('today', now)), {fromSecs: at(2026, 9, 27), toSecs: at(2026, 9, 28)});
assert.deepEqual(plain(period.periodRange('yesterday', now)), {fromSecs: at(2026, 9, 26), toSecs: at(2026, 9, 27)});
assert.deepEqual(plain(period.periodRange('month', now)), {fromSecs: at(2026, 9, 1), toSecs: at(2026, 10, 1)});
assert.deepEqual(plain(period.periodRange('lastMonth', now)), {fromSecs: at(2026, 8, 1), toSecs: at(2026, 9, 1)});
assert.deepEqual(plain(period.periodRange('all', now)), {}, 'the whole kept ledger');
// Across a year and a month's end.
assert.deepEqual(plain(period.periodRange('lastMonth', new Date(2026, 0, 15))), {fromSecs: at(2025, 12, 1), toSecs: at(2026, 1, 1)});
assert.deepEqual(plain(period.periodRange('yesterday', new Date(2026, 2, 1, 8))), {fromSecs: at(2026, 2, 28), toSecs: at(2026, 3, 1)});
assert.deepEqual(plain(period.periodRange('today', new Date(2026, 11, 31, 23, 59))), {fromSecs: at(2026, 12, 31), toSecs: at(2027, 1, 1)});
// Chosen days, both included; not a period when a date is missing, invalid or before the first.
assert.deepEqual(plain(period.periodRange('custom', now, {from: '2026-09-01', to: '2026-09-15'})), {fromSecs: at(2026, 9, 1), toSecs: at(2026, 9, 16)});
assert.deepEqual(plain(period.periodRange('custom', now, {from: '2026-09-15', to: '2026-09-15'})), {fromSecs: at(2026, 9, 15), toSecs: at(2026, 9, 16)});
for (const custom of [{from: '2026-09-16', to: '2026-09-15'}, {from: '', to: '2026-09-15'}, {from: '2026-02-30', to: '2026-03-02'}, {from: '2026/09/01', to: '2026-09-02'}])
  assert.equal(period.periodRange('custom', now, custom), null, JSON.stringify(custom));
assert.equal(period.periodText({}), '累计（全部保留账本）');
assert.equal(period.periodText({fromSecs: at(2026, 9, 27), toSecs: at(2026, 9, 28)}), '2026-09-27');
assert.equal(period.periodText({fromSecs: at(2026, 9, 1), toSecs: at(2026, 10, 1)}), '2026-09-01 至 2026-09-30');
assert.equal(period.dateInput(now), '2026-09-27');
console.log('PASS: 今天, 昨天, 本月, 上月 and chosen days as local [from, to) bounds, across month and year ends; invalid choices are no period');

// CSV records as RFC 4180 has them: quoted cells may hold commas, quotes and line breaks.
const records = period.csvRecords('id,ts,reason\n"a",10,"补偿, 9/26"\n"b",20,"说 ""好"""\r\n"c",30,"两行\n原因"\n');
assert.deepEqual(plain(records.map(record => record.cells)), [['id', 'ts', 'reason'], ['a', '10', '补偿, 9/26'], ['b', '20', '说 "好"'], ['c', '30', '两行\n原因']]);
// The ledger cut to a period by its ts column (from inclusive, to exclusive), each entry kept byte for byte.
const csv = 'id,card_id,ts,kind\n"e1","card-1",100,"usage"\n"e2","card-1",200,"adjustment"\n"e3","card-2",300,"usage"\n';
assert.deepEqual(plain(period.ledgerForPeriod(csv, {fromSecs: 200, toSecs: 300})), {csv: 'id,card_id,ts,kind\n"e2","card-1",200,"adjustment"\n', entries: 1});
assert.deepEqual(plain(period.ledgerForPeriod(csv, {fromSecs: 150})), {csv: 'id,card_id,ts,kind\n"e2","card-1",200,"adjustment"\n"e3","card-2",300,"usage"\n', entries: 2});
assert.deepEqual(plain(period.ledgerForPeriod(csv.replace(/\n/g, '\r\n'), {toSecs: 101})), {csv: 'id,card_id,ts,kind\r\n"e1","card-1",100,"usage"\r\n', entries: 1}, 'CRLF kept');
assert.deepEqual(plain(period.ledgerForPeriod('id,ts\n"x",5\n"y, with a\nbreak",6\n', {fromSecs: 6})), {csv: 'id,ts\n"y, with a\nbreak",6\n', entries: 1}, 'a multi-line cell stays whole');
assert.equal(period.ledgerForPeriod('id,points\nfixture,27\n', {fromSecs: 1}), null, 'no ts column');
assert.deepEqual(plain(period.ledgerForPeriod('id,ts\n', {fromSecs: 1})), {csv: 'id,ts\n', entries: 0});
console.log('PASS: the ledger CSV cut to a period keeps its header and each entry exactly, quoted cells with commas and line breaks included');
