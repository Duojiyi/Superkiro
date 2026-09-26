// Rules behind finding a card from its code and the console's page addresses, without a browser.
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
  vm.runInNewContext(code, {exports, require: name => imports[name], TextEncoder, URLSearchParams, crypto: globalThis.crypto});
  return exports;
}
// Values made inside a module's context compare by content, not by prototype.
const plain = value => JSON.parse(JSON.stringify(value));

(async () => {
  // A card's ID from the code a customer quotes: normalised as crates/billing/src/card.rs does,
  // SHA-256, card- and 16 hex digits. The IDs are the ones the Rust test
  // test_card_id_is_the_start_of_the_code_hash (crates/billing/tests/card_template_test.rs) pins.
  const code = load('cardCode.ts');
  const id = async text => {const found = code.cardCodeOf(text); return found && code.cardIdForCode(found);};
  assert.equal(code.normalizeCardCode('  kiro 9F83-A1B2 　'), 'kiro9f83a1b2', 'trimmed like Rust, ASCII lower case, dashes and spaces gone');
  assert.equal(code.normalizeCardCode('﻿kiro'), '﻿kiro', 'Rust does not trim a byte-order mark');
  assert.equal(code.normalizeCardCode('Kiro'), 'Kiro', 'only ASCII letters are lowered');
  for (const [text, expected] of [
    ['kiro-9F83-A1B2-C3D4-E5F6-7A8B-9C0D-1E2F-3A4B', 'card-74f48022ecefbdeb'],
    ['  kiro 9f83 a1b2 c3d4 e5f6 7a8b 9c0d 1e2f 3a4b  ', 'card-74f48022ecefbdeb'],
    ['KIRO9F83A1B2C3D4E5F67A8B9C0D1E2F3A4B', 'card-74f48022ecefbdeb'],
    ['kiro-3eba-7810-bb00-a2e3-465a-fcc8-b9de-7471', 'card-b802b003e19a4e48'],
    ['kiro-0000-0000-0000-0000-0000-0000-0000-0001', 'card-b48b34bd2116ba5a'],
    // The hex alone: the kiro every code starts with is put back before hashing.
    ['3EBA 7810 BB00 A2E3 465A FCC8 B9DE 7471', 'card-b802b003e19a4e48'],
  ]) assert.equal(await id(text), expected, text);
  for (const text of ['', 'kiro-3eba', 'kiro-3eba-7810-bb00-a2e3-465a-fcc8-b9de-747', 'kiro-3eba-7810-bb00-a2e3-465a-fcc8-b9de-74711', 'kiro-3eba-7810-bb00-a2e3-465a-fcc8-b9de-747g',
    'card-b802b003e19a4e48', 'kiro_3eba_7810_bb00_a2e3_465a_fcc8_b9de_7471', 'kiro-3eba-7810\tbb00-a2e3-465a-fcc8-b9de-7471'])
    assert.equal(code.cardCodeOf(text), null, `not a whole code: ${text}`);
  // Kept out of addresses, even half typed; card IDs, devices, notes and batch references are not codes.
  for (const text of ['kiro-3', 'kiro-3eba-78', 'KIRO 3EBA', '3eba-7810', '3eba7810bb00a2e34', '3eba 7810 bb00'])
    assert(code.looksLikeCardCode(text), `code-like: ${text}`);
  for (const text of ['kiro', 'card-b802b003e19a4e48', 'b802b003', 'b802b003e19a4e48', 'fixture-device-0', 'dev-5616…7828', '淘宝 9 月', '批次 2026-09-20T15:02:58.534Z 48cb0b82-d772'])
    assert(!code.looksLikeCardCode(text), `not code-like: ${text}`);
  console.log('PASS card codes: normalised as card.rs, IDs equal to those the Rust code derives, the hex alone accepted, partial codes recognised');

  // Page addresses: what each page keeps, defaults left out, values checked, never a card code.
  const route = load('route.ts', {'./cardCode': code});
  const parse = hash => plain(route.parseRoute(hash));
  assert.deepEqual(parse(''), {tab: 'overview', params: {}});
  assert.deepEqual(parse('#/nowhere?q=x'), {tab: 'overview', params: {}}, 'an unknown page is 运营概览');
  assert.deepEqual(parse('#/finance'), {tab: 'reconciliation', params: {}});
  assert.deepEqual(parse('#/models?edit=x&open=y'), {tab: 'models', params: {}}, 'the models page keeps only the page');
  assert.deepEqual(parse('#/cards?q=%E6%B7%98%E5%AE%9D+9+%E6%9C%88&status=FROZEN&quick=low&group=fixture-group-1&open=card-b802b003e19a4e48&x=1'),
    {tab: 'cards', params: {q: '淘宝 9 月', status: 'FROZEN', quick: 'low', group: 'fixture-group-1', open: 'card-b802b003e19a4e48'}});
  assert.deepEqual(parse('#/cards?status=CURRENT&quick=soon'), {tab: 'cards', params: {}}, 'the default tab and unknown values are dropped');
  assert.deepEqual(parse('#/cards?q=kiro-3eba-7810-bb00-a2e3-465a-fcc8-b9de-7471'), {tab: 'cards', params: {}}, 'a card code in an address is ignored');
  assert.deepEqual(parse('#/traces?card=card-1&status=error&reason=&range=day&open=trace-9'),
    {tab: 'traces', params: {card: 'card-1', status: 'error', range: 'day', reason: '', open: 'trace-9'}}, 'an empty reason is 未分类');
  assert.deepEqual(parse('#/providers/?key=hanyue-max-key-1'), {tab: 'providers', params: {key: 'hanyue-max-key-1'}});
  const hash = (tab, intent) => route.routeHash(route.routeOf(tab, intent));
  assert.equal(hash('cards', {cards: {status: 'CURRENT', search: '  淘宝 9 月 ', group: 'ALL', open: 'card-1'}}), '#/cards?q=%E6%B7%98%E5%AE%9D+9+%E6%9C%88&open=card-1');
  assert.equal(hash('cards', {cards: {search: 'kiro-3eba-7810-bb00'}}), '#/cards', 'a code being typed never reaches the address');
  assert.equal(hash('traces', {traces: {status: 'ALL', window: 'all', search: 'card-1', card: 'card-1', model: 'ALL', provider: 'ALL', open: 'trace-2'}}), '#/traces?card=card-1&open=trace-2');
  assert.equal(hash('traces', {traces: {search: 'trace-9', status: 'error', reason: 'no_route'}}), '#/traces?q=trace-9&status=error&reason=no_route');
  assert.equal(hash('providers', {providers: {key: 'k1', edit: 'k2'}}), '#/providers?key=k1&edit=k2');
  assert.equal(hash('reconciliation'), '#/finance');
  for (const [tab, intent] of [['cards', {cards: {status: 'EXPIRED', quick: 'expiring', search: 'x y', open: 'card-2'}}], ['traces', {traces: {status: 'in_progress', window: 'hour', search: 'abc', provider: 'p', open: 't'}}]]) {
    const again = route.intentOf(route.parseRoute(hash(tab, intent)));
    assert.equal(hash(tab, again), hash(tab, intent), `${tab}: an address read back gives the same address`);
  }
  assert.equal(route.OPEN_PARAM.cards, 'open');assert.equal(route.OPEN_PARAM.providers, 'edit');
  console.log('PASS page addresses: pages and their parameters, defaults left out, unknown values dropped, card codes never kept, read back unchanged');

  // The saved ledger's size: soon from the server's warning level, now from a quarter of the
  // ceiling (billing's STATE_URGENT_BYTES, where the server logs "archive … now").
  const status = load('status.ts'), display = load('format.ts');
  const MB = 1048576, stats = bytes => ({stateBytes: bytes, stateWarningBytes: 32 * MB, stateCeilingBytes: 256 * MB});
  assert.deepEqual([12 * MB, 32 * MB - 1, 32 * MB, 64 * MB - 1, 64 * MB, 300 * MB].map(bytes => status.storageLevel(stats(bytes)).level), ['ok', 'ok', 'soon', 'soon', 'now', 'now']);
  assert.equal(status.storageLevel(stats(1)).urgent, 64 * MB);
  assert.equal(status.storageLevel({}), null, 'an older server that reports no size raises nothing');
  assert.equal(status.storageLevel(null), null);
  assert.deepEqual([0, 900, 820 * 1024, 71.25 * MB, 256 * MB, undefined].map(display.formatBytes), ['0 KB', '1 KB', '820 KB', '71.3 MB', '256 MB', '—']);
  console.log('PASS ledger storage: levels at 32 MB and 64 MB of 256 MB, sizes in KB and MB');
})().catch(error => {console.error(error); process.exitCode = 1;});
