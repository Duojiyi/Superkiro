const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const ts = require('typescript');
const vm = require('node:vm');
function load(file, globals = {}) {
  const source = fs.readFileSync(path.join(__dirname, '../src', file), 'utf8');
  const exports = {};
  vm.runInNewContext(ts.transpileModule(source, {compilerOptions: {module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022}}).outputText, {exports, Headers, AbortController, setTimeout, clearTimeout, ...globals});
  return exports;
}
const {pointsToMicro} = load('pricing.ts');
for (const [input, expected] of [['0', 0], ['120', 120000000], ['0.000001', 1], ['1.000001', 1000001], ['12.34', 12340000]]) assert.equal(pointsToMicro(input), expected);
for (const input of ['', '-1', 'NaN', 'Infinity', '1e3', '1.0000001', '9007199254.740992']) assert.throws(() => pointsToMicro(input));
(async () => {
  const calls = [];
  const {AdminApiClient} = load('api.ts', {fetch: async (url, options = {}) => {
    calls.push({url, options});
    return {ok: true, json: async () => url.endsWith('/session') && options.method === 'POST' ? {success: true, expiresIn: 900} : {success: true, role: 'admin', csrfToken: 'test-csrf', cards: [], rawCode: 'recovered-code'}};
  }});
  const api = new AdminApiClient('');
  await api.establishSession('admin', 'test-password');
  assert.deepEqual(JSON.parse(calls[0].options.body), {username: 'admin', password: 'test-password'});
  assert.equal(calls[0].options.headers.has('x-admin-key'), false);
  for (const templateId of ['tier-1000', 'tier-2000', 'tier-5000', 'tier-10000']) {
    const group = 'arbitrary-billing-group';
    await api.batchCards(2, group, templateId);
    const call = calls.at(-1);
    assert.equal(call.url, '/api/v1/admin/cards/batch');
    assert.deepEqual(JSON.parse(call.options.body), {count: 2, groupId: group, templateId, maxDevices: 1});
    assert.equal(call.options.headers.has('Authorization'), false);
    assert.equal(call.options.headers.get('x-csrf-token'), 'test-csrf');
    assert.equal(call.options.credentials, 'same-origin');
    assert.equal(call.options.cache, 'no-store');
  }
  for (const group of [undefined, '', '   ']) await assert.rejects(() => api.batchCards(1, group), /请选择模型与计费分组/);
  await api.publishCommercialConfig({expected_revision: 'revision-test', reason: 'test', groups: []});
  assert.equal(JSON.parse(calls.at(-1).options.body).expected_revision, 'revision-test');
  assert.equal(calls.at(-1).url, '/api/v1/admin/commercial-config');
  assert.equal((await api.revealCard('card-1')).rawCode, 'recovered-code');
  assert.equal(calls.at(-1).url, '/api/v1/admin/cards/reveal');
  assert.deepEqual(JSON.parse(calls.at(-1).options.body), {cardId: 'card-1'});
  for (const [run, endpoint, expected] of [
    [() => api.adjustBalance('card-1', -10, 'refund'), '/cards/adjust', {cardId:'card-1',deltaPoints:-10,reason:'refund'}],
    [() => api.updateCardStatus('card-1', 'ban', 'abuse'), '/cards/status', {cardId:'card-1',action:'ban',reason:'abuse'}],
    [() => api.updateCardStatus('card-2', 'void', 'unused inventory'), '/cards/status', {cardId:'card-2',action:'void',reason:'unused inventory'}],
    [() => api.updateCardStatus('card-2', 'archive', 'cleanup'), '/cards/status', {cardId:'card-2',action:'archive',reason:'cleanup'}],
    [() => api.updateCardStatus('card-2', 'unarchive', 'review'), '/cards/status', {cardId:'card-2',action:'unarchive',reason:'review'}],
    [() => api.createAnnouncement('notice', 'body', 'warning', 3600), '/announcements', {title:'notice',content:'body',level:'warning',ttlSecs:3600}],
    [() => api.updateProviderStatus('provider-1', false), '/providers/status', {providerId:'provider-1',enabled:false}],
    [() => api.pruneTraces(123), '/traces/prune', {cutoffSecs:123}],
    [() => api.manageKey('save', {provider_id:'p',key_id:'k',allowed_models:[]}), '/providers/keys', {provider_id:'p',key_id:'k',allowed_models:[]}],
    [() => api.manageKey('discover', {provider_id:'p',key_id:'k'}), '/providers/keys/discover', {provider_id:'p',key_id:'k'}],
    [() => api.importProvider({providers:[]}), '/providers/import', {format:'cc_switch',content:{providers:[]}}],
  ]) {
    await run(); assert.equal(calls.at(-1).url, '/api/v1/admin'+endpoint);
    assert.deepEqual(JSON.parse(calls.at(-1).options.body),expected);
    assert.equal(calls.at(-1).options.headers.get('x-csrf-token'),'test-csrf');
  }
  await api.logout(true);
  assert.equal(calls.at(-1).options.headers.get('x-csrf-token'), 'test-csrf');
  let unauthorized = false;
  const {AdminApiClient: ErrorClient} = load('api.ts', {fetch: async () => ({ok: false, status: 401, json: async () => ({error: 'expired'})})});
  const expired = new ErrorClient(); expired.onUnauthorized = () => {unauthorized = true;};
  await assert.rejects(expired.getStats(), /请先登录/); assert.equal(unauthorized, false);
  await assert.rejects(expired.checkAuth(), error=>error.name==='AdminApiError'&&error.status===401&&error.message==='expired'); assert.equal(unauthorized, true);
  await expired.logout(); // 401 confirms there is no remaining authenticated session.
  let pageCount = 0;
  const {AdminApiClient: PagedClient} = load('api.ts', {fetch: async (url) => ({ok:true, json:async () => url.endsWith('/session') ? {success:true,role:'admin',csrfToken:'test-csrf'} : ({success:true,cards:pageCount++ === 0 ? Array.from({length:500}, (_,i)=>({id:String(i)})) : [{id:'last'}]})})});
  const paged = new PagedClient(); await paged.checkAuth();
  const pages = await paged.getCards(); assert.equal(pages.cards.length,501); assert.equal(pageCount,2);
  console.log('PASS: exact price conversion, invalid price rejection, cookie/CSRF, expiry/logout failures, pagination, reveal, daily-operation payloads, four tiers and revision publishing');
})().catch(error => {console.error(error); process.exitCode = 1;});

// Persist only the operator-bound, non-credential adjustment intent.
{
 const {loadAdjustment,saveAdjustment,clearAdjustment}=load('adjustment.ts');
 const values=new Map(), storage={getItem:k=>values.get(k)??null,setItem:(k,v)=>values.set(k,v),removeItem:k=>values.delete(k)};
 const intent={operator:'admin',cardId:'card-1',delta:10,reason:'compensation',key:'intent-1'};
 saveAdjustment(storage,{...intent,password:'never persist',token:'secret'});
 assert.deepEqual(JSON.parse(JSON.stringify(loadAdjustment(storage,'admin'))),intent);
 assert.equal(loadAdjustment(storage,'other-admin'),null);
 assert(![...values.values()][0].includes('secret'));assert(![...values.values()][0].includes('password'));
 saveAdjustment(storage,intent);assert.throws(()=>saveAdjustment(storage,{...intent,key:'different'}));
 clearAdjustment(storage,{...intent,key:'different'});assert.equal(values.size,1);
 clearAdjustment(storage,intent);assert.equal(values.size,0);
 assert.throws(()=>saveAdjustment({...storage,setItem(){throw Error('denied');}},intent));
 values.set('superkiro.pending-adjustment.v1:admin','invalid');assert.throws(()=>loadAdjustment(storage,'admin'));
 console.log('PASS adjustment storage: whitelist, operator isolation, stable retry, clear, corrupt/denied');
}
{
 const {parseFinancialSettings,financialEstimates,estimatedMoney}=load('financial.ts');
 assert.deepEqual(JSON.parse(JSON.stringify(parseFinancialSettings('0.01','7.2'))),{credit_face_value_cny:0.01,usd_cny_rate:7.2});
 for(const value of ['', '0', '-1', 'Infinity', '1001'])assert.throws(()=>parseFinancialSettings(value,'7'));
 const data={basis:'retained_usage_ledger_estimate_not_cash_revenue',estimates:{retainedLedgerOnly:true,costedRequests:2,uncostedRequests:1,faceValueLessCostMicroCny:123,faceValueMarginPercentage:50}};
 assert.equal(financialEstimates(data).faceValueLessCostMicroCny,null);assert.equal(financialEstimates(data).faceValueMarginPercentage,null);
 assert.equal(financialEstimates({dashboard:{provider_cost_micro_cny:100}}),null);assert.equal(estimatedMoney(null),'未提供');assert.equal(estimatedMoney(1000000),'1 元');
 assert.equal(financialEstimates({...data,estimates:{...data.estimates,uncostedRequests:0,faceValueMarginPercentage:null}}).faceValueMarginPercentage,null);
 console.log('PASS financial settings bounds, micro-CNY units, incomplete coverage, no legacy fallback');
}

{
 const {adjustmentPointsToMicro}=load('pricing.ts');
 for(const [input,micro] of [['0.000001',1],['-0.000001',-1],['1.000001',1000001],['1000000',1000000000000],['-1000000',-1000000000000]])assert.equal(adjustmentPointsToMicro(input),micro);
 for(const input of ['0.0000001','-0.0000001','1.0000001','1e-7','0','-0','1000001','NaN','Infinity'])assert.throws(()=>adjustmentPointsToMicro(input));
 const {isZeroMicroAdjustment,isUnsubmittedAdjustmentRejection,saveAdjustment,loadAdjustment,clearAdjustment}=load('adjustment.ts');
 const old={operator:'admin',cardId:'card-1',delta:0.0000001,reason:'legacy',key:'old-zero-micro'};
 const values=new Map([['superkiro.pending-adjustment.v1:admin',JSON.stringify(old)]]),storage={getItem:k=>values.get(k)??null,setItem:(k,v)=>values.set(k,v),removeItem:k=>values.delete(k)};
 assert(isZeroMicroAdjustment(loadAdjustment(storage,'admin')));
 assert(isUnsubmittedAdjustmentRejection(400,'Invalid billing state: cannot adjust a voided card',old));
 assert.equal(isUnsubmittedAdjustmentRejection(409,'Invalid billing state: cannot adjust a voided card',old),false);
 assert.equal(isUnsubmittedAdjustmentRejection(400,'Invalid billing state: unknown',old),false);
 assert.throws(()=>saveAdjustment({...storage,getItem:()=>null},old));
 saveAdjustment(storage,old); // Exact legacy retries remain recoverable; UI offers explicit zero-micro clear.
 const message='Invalid balance adjustment: Adjustment delta cannot be zero';
 const insufficient='Card error: Insufficient credit: available 1000000 micro-credits, needed 2000000';
 assert(isUnsubmittedAdjustmentRejection(409,insufficient,{...old,delta:-2}));
 for(const [status,text,delta] of [[503,insufficient,-2],[409,'Idempotency conflict',-2],[409,insufficient,2],[409,insufficient,-3]])assert.equal(isUnsubmittedAdjustmentRejection(status,text,{...old,delta}),false);
 assert(isUnsubmittedAdjustmentRejection(400,message,old));assert(isUnsubmittedAdjustmentRejection(400,message,{...old,delta:-0.0000001}));
 for(const [status,text,intent] of [[400,'another error',old],[409,message,old],[503,message,old],[400,message,{...old,delta:10}],[400,message,{...old,delta:0.0000005}]])assert.equal(isUnsubmittedAdjustmentRejection(status,text,intent),false);
 clearAdjustment(storage,old);assert.equal(values.size,0);
 const roundedLegacy={...old,delta:0.0000009};values.set('superkiro.pending-adjustment.v1:admin',JSON.stringify(roundedLegacy));saveAdjustment(storage,roundedLegacy);assert.equal(loadAdjustment(storage,'admin').delta,0.0000009);
 console.log('PASS signed microcredit precision/bounds, legacy zero-micro recovery, narrow rejection whitelist');
}
