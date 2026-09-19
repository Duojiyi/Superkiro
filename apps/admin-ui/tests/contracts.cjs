const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const ts = require('typescript');
const vm = require('node:vm');
function load(file, globals = {}) {
  const source = fs.readFileSync(path.join(__dirname, '../src', file), 'utf8');
  const exports = {};
  vm.runInNewContext(ts.transpileModule(source, {compilerOptions: {module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022}}).outputText, {exports, Headers, ...globals});
  return exports;
}
const {pointsToMicro} = load('pricing.ts');
for (const [input, expected] of [['0', 0], ['120', 120000000], ['0.000001', 1], ['1.000001', 1000001], ['12.34', 12340000]]) assert.equal(pointsToMicro(input), expected);
for (const input of ['', '-1', 'NaN', 'Infinity', '1e3', '1.0000001', '9007199254.740992']) assert.throws(() => pointsToMicro(input));
(async () => {
  const calls = [];
  const {AdminApiClient} = load('api.ts', {fetch: async (url, options = {}) => {
    calls.push({url, options});
    return {ok: true, json: async () => url.endsWith('/session') ? {accessToken: 'test-session', tokenType: 'Bearer', expiresIn: 60, expiresAt: 60} : {success: true, cards: []}};
  }});
  const api = new AdminApiClient('', 'test-key');
  await api.establishSession();
  assert.equal(api.getAdminKey(), '');
  for (const templateId of ['tier-1000', 'tier-2000', 'tier-5000', 'tier-10000']) {
    const group = 'group-pro-plus';
    await api.batchCards(2, group, templateId);
    const call = calls.at(-1);
    assert.equal(call.url, '/api/v1/admin/cards/batch');
    assert.deepEqual(JSON.parse(call.options.body), {count: 2, groupId: group, templateId, maxDevices: 1});
    assert.equal(call.options.headers.get('Authorization'), 'Bearer test-session');
  }
  await api.publishCommercialConfig({expected_revision: 'revision-test', reason: 'test', groups: []});
  assert.equal(JSON.parse(calls.at(-1).options.body).expected_revision, 'revision-test');
  assert.equal(calls.at(-1).url, '/api/v1/admin/commercial-config');
  console.log('PASS: exact price conversion, invalid price rejection, memory-only session, four tier payloads, revision publishing contract');
})().catch(error => {console.error(error); process.exitCode = 1;});
