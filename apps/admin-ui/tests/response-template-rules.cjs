// Offline response-template boundaries and the shared authenticated API contract.
const assert = require('node:assert/strict');
const fs = require('node:fs'), path = require('node:path'), vm = require('node:vm'), ts = require('typescript');
const plain = value => JSON.parse(JSON.stringify(value));
function load(file, extra = {}) {
  const exports = {};
  const code = ts.transpileModule(fs.readFileSync(path.join(__dirname, '../src', file), 'utf8'), {compilerOptions: {module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022}}).outputText;
  vm.runInNewContext(code, {exports, require: name => {if (name === './pelicanTemplate') return load('pelicanTemplate.ts'); throw new Error('Unexpected import: ' + name);}, TextEncoder, crypto: require('node:crypto').webcrypto, Headers, AbortController, setTimeout, clearTimeout, ...extra});
  return exports;
}
const t = load('responseTemplates.ts');
const valid = () => ({...t.newTemplateRule(), name: '规则', match_text: '  原样匹配  ', variants: [{...t.newTemplateVariant(), model_id: 'gpt-test', content: '<!doctype html><script>alert(1)</script>'}]});
assert.deepEqual(plain(t.parseTemplateRules([])), {rules: []}, 'initial empty config stays empty');
assert.equal(t.newTemplateRule().enabled, false);
assert.equal(t.newTemplateVariant().price_credits, '0');
for (const [input, expected] of [['0',0],['0.000001',1],['0.123456',123456],['1.000001',1000001],['999.999999',999999999],['1000',1000000000]]) {
  assert.equal(t.templatePrice(input), expected); assert.equal(t.templatePrice(t.templateCredits(expected)), expected);
}
for (const input of ['', ' ', '-1', '+1', '.1', '1.', '1e2', '0.0000001', '1000.000001', 'Infinity', 'NaN', '9007199254740993']) assert.equal(t.templatePrice(input), null, input);
for (const path of ['index.html','index.htm','index.HTML','pages/demo.html','示例/页面.html']) assert.equal(t.safeTemplatePath(path),true,path);
for (const path of ['', 'foo.txt', '/a.html','../a.html','x/../a.html','./a.html','x//a.html','C:/a.html','C:\\a.html','\\\\server\\a.html','https://x/a.html','%2e%2e/a.html','a.html?x','a.html#x','x\u0000.html','a.html ','CON.html','CON .html','CONIN$.html','COM¹.html','x'.repeat(241)+'.html','aux/page.html','x./page.html','x /page.html']) assert.equal(t.safeTemplatePath(path),false,path);
for (const file_path of ['index.htm', 'index.HTML']) {
  const draft = valid(); draft.enabled = false; draft.variants[0].file_path = file_path;
  assert(t.parseTemplateRules([draft]).rules, 'existing valid paths can be disabled');
}
let rule = valid();
let parsed = t.parseTemplateRules([rule]);
assert.equal(parsed.rules[0].match_text, '  原样匹配  ', 'do not silently normalize match semantics');
assert.equal(parsed.rules[0].variants[0].content, rule.variants[0].content, 'HTML saved as opaque text');
assert.equal(parsed.rules[0].variants[0].price_microcredits,0);
assert(!('price_credits' in parsed.rules[0].variants[0]));
assert.deepEqual(plain(t.templateDrafts(parsed.rules)),plain([rule]));
rule.variants.push({...rule.variants[0]}); assert.match(t.parseTemplateRules([rule]).error,/不能重复/);
rule=valid();rule.variants[0].price_credits='';assert.match(t.parseTemplateRules([rule]).error,/最多 6 位小数/);
rule=valid();rule.variants=[];assert.match(t.parseTemplateRules([rule]).error,/1–32/);
rule=valid();rule.variants=Array.from({length:33},(_,i)=>({...rule.variants[0],model_id:`m-${i}`}));assert.match(t.parseTemplateRules([rule]).error,/1–32/);
assert.match(t.parseTemplateRules(Array.from({length:33},()=>valid())).error,/32 条/);
rule=valid();assert.match(t.parseTemplateRules([rule,rule]).error,/ID/);
rule=valid();rule.match_text=' ';assert.match(t.parseTemplateRules([rule]).error,/匹配文本/);
rule=valid();rule.match_mode='regex';assert.match(t.parseTemplateRules([rule]).error,/完整匹配/);
rule=valid();rule.variants[0].content='a'.repeat(256*1024);assert(t.parseTemplateRules([rule]).rules);
rule.variants[0].content+='a';assert.match(t.parseTemplateRules([rule]).error,/256 KiB/);
rule.variants[0].content='中'.repeat(90000);assert.match(t.parseTemplateRules([rule]).error,/256 KiB/,'UTF-8 bytes not JS characters');
rule=valid();rule.variants=Array.from({length:9},(_,i)=>({...rule.variants[0],model_id:`m-${i}`,content:'a'.repeat(256*1024)}));assert.match(t.parseTemplateRules([rule]).error,/2 MiB/);
rule=valid();rule.variants[0].preamble='中'.repeat(700000);assert.match(t.parseTemplateRules([rule]).error,/4096 字节/,'messages bounded before total');
const first=valid(),second=valid();second.match_mode='contains';assert.deepEqual(plain(t.parseTemplateRules([second,first]).rules.map(x=>x.id)),[second.id,first.id]);
assert.deepEqual(plain(t.templateModels([{id:'internal',exposed_model_id:'visible'},{id:'other',exposed_model_id:'visible'},{id:'not-client-visible'}])),[{id:'visible',label:'visible'}]);
const example=t.pelicanTemplateRule();assert.equal(example.enabled,false);assert.equal(example.variants[0].price_credits,'0');assert.equal(example.variants[0].model_id,'claude-opus-5-5');assert(example.variants[0].content.includes('鹈鹕'));
console.log('PASS: template validation, UTF-8 limits, safe paths, exact microcredits, unique models and inert/disabled/free defaults');
(async()=>{
  const calls=[];let status=200;
  const {AdminApiClient,AdminApiError}=load('api.ts',{fetch:async(url,options)=>{
    calls.push({url,options});
    if(url.endsWith('/session'))return {ok:true,json:async()=>({success:true,role:'admin',csrfToken:'test-csrf'})};
    return {ok:status===200,status,json:async()=>status===200?{success:true,config:{revision:'r1',rules:[],audit:[]}}:{error:'revision conflict'}};
  }});
  const api=new AdminApiClient();
  await assert.rejects(api.getResponseTemplates(),/请先登录/);assert.equal(calls.length,0);
  await api.checkAuth();
  await api.getResponseTemplates();
  const update={expected_revision:'r0',reason:'测试',rules:[]};
  await api.publishResponseTemplates(update);
  const get=calls[1],post=calls[2];
  assert.equal(get.url,'/api/v1/admin/response-templates');assert.equal(post.url,get.url);
  assert.equal(post.options.method,'POST');assert.equal(post.options.headers.get('x-csrf-token'),'test-csrf');
  assert.equal(post.options.credentials,'same-origin');assert.equal(post.options.headers.get('Content-Type'),'application/json');
  assert.deepEqual(JSON.parse(post.options.body),update);
  status=409;await assert.rejects(api.publishResponseTemplates(update),error=>error instanceof AdminApiError&&error.status===409);
  assert.equal(calls.length,4,'conflicts are not automatically retried');
  console.log('PASS: response-template GET/POST reuse auth, CSRF, snake_case and typed conflict handling');
})().catch(error=>{console.error(error);process.exitCode=1;});

for (const delay of [-1, 30001, 0.1, Infinity, NaN]) {const d=valid();d.variants[0].delay_ms=delay;assert.ok(t.parseTemplateRules([d]).error);}
const opusExample=t.pelicanTemplateRule();assert.equal(opusExample.variants[0].model_id,'claude-opus-5-5');assert.equal(opusExample.variants[0].delay_ms,1500);assert.ok(opusExample.variants[0].content.includes('<svg'));
console.log('PASS: template delay bounds and embedded Opus 5.5 HTML');

const intentDraft = valid(); intentDraft.match_mode = 'intent'; intentDraft.intent = t.pelicanIntent();
assert.ok(t.parseTemplateRules([intentDraft]).rules);
intentDraft.intent.groups = [[]]; assert.ok(t.parseTemplateRules([intentDraft]).error);
const timelineDraft = valid(); timelineDraft.variants[0].delivery = t.newTemplateDelivery();
assert.ok(t.parseTemplateRules([timelineDraft]).rules);
for (const patch of [{write_min_ms: -1}, {write_max_ms: 300001}, {write_min_ms: 240000}, {messages: [{at_ms: 0, text: 'a'}, {at_ms: 0, text: 'b'}]}, {messages: [{at_ms: 240000, text: 'late'}]}]) {
 timelineDraft.variants[0].delivery = {...t.newTemplateDelivery(), ...patch}; assert.ok(t.parseTemplateRules([timelineDraft]).error);
}
console.log('PASS: intent groups and bounded editable timeline validation');

assert.deepEqual(plain(t.pelicanIntent()), JSON.parse(fs.readFileSync(path.join(__dirname, '../../../crates/billing/tests/fixtures/pelican_intent.json'), 'utf8')), 'admin defaults must match production matcher regression fixture');

const saved = t.parseTemplateRules([valid()]).rules;
const reordered = saved.map(r => Object.fromEntries(Object.entries({...r, intent: null, variants: r.variants.map(v => ({...v, delivery: null}))}).reverse()));
assert(t.sameTemplateRules(saved,reordered), 'uncertain save compares semantic rules regardless of JSON order or omitted optional fields');
reordered[0].variants[0].price_microcredits = 1;
assert(!t.sameTemplateRules(saved,reordered), 'uncertain save must detect a different charge');

const omitted = JSON.parse(JSON.stringify(saved));
delete omitted[0].intent; delete omitted[0].variants[0].delivery;
assert(t.sameTemplateRules(saved,omitted), 'absent optional fields preserve save equality');

const randomDraft = valid(); randomDraft.variants[0].delivery = {...t.newTemplateDelivery(), write_min_ms:110000, write_max_ms:130000, messages:[{at_ms:10000,text:'first'},{at_ms:50000,at_max_ms:80000,text:'second'}]};
assert.ok(t.parseTemplateRules([randomDraft]).rules);
for (const at_max_ms of [49999,110001,-1,80000.5,NaN,Infinity]) {
 const d=structuredClone(randomDraft); d.variants[0].delivery.messages[1].at_max_ms=at_max_ms; assert.ok(t.parseTemplateRules([d]).error, String(at_max_ms));
}
const overlap=structuredClone(randomDraft);overlap.variants[0].delivery.messages[0].at_max_ms=50000;assert.ok(t.parseTemplateRules([overlap]).error);
const fixed=t.parseTemplateRules([randomDraft]).rules;const nullable=structuredClone(fixed);nullable[0].variants[0].delivery.messages[0].at_max_ms=null;
assert(t.sameTemplateRules(fixed,nullable),'null and omitted upper bound are equivalent');
assert.equal(opusExample.variants[0].file_path,'pelican-bicycle.html');
assert(!/<script\b|@keyframes|https?:\/\//i.test(opusExample.variants[0].content));
assert(opusExample.variants[0].content.includes('<animateTransform'));
console.log('PASS: bounded random message windows and legacy fixed-time compatibility');


assert.equal(t.adjustTemplateSelection(3, 1), 2, 'deleting an earlier candidate shifts an explicit selection');
assert.equal(t.adjustTemplateSelection(1, 1), undefined, 'deleting the selected candidate clears selection');
assert.equal(t.adjustTemplateSelection(0, 1), 0, 'deleting a later candidate keeps selection');
const codeCandidates = valid();
codeCandidates.variants[0].content_alternatives = [
  {file_path: 'pages/second.html', content: '<html>second</html>'},
  {file_path: 'pages/third.html', content: '<html>third</html>'}
];
for (const selected of [undefined, 0, 1, 2]) {
  const d = structuredClone(codeCandidates);
  d.variants[0].content_alternative_index = selected;
  assert.ok(t.parseTemplateRules([d]).rules, `code candidate selection ${selected}`);
}
for (const bad of [3, -1, 1.5, NaN]) {
  const d = structuredClone(codeCandidates); d.variants[0].content_alternative_index = bad;
  assert.ok(t.parseTemplateRules([d]).error, `invalid code candidate selection ${bad}`);
}
const unsafeCode = structuredClone(codeCandidates); unsafeCode.variants[0].content_alternatives[0].file_path = '../escape.html';
assert.ok(t.parseTemplateRules([unsafeCode]).error, 'extra code candidate paths use the same traversal guard');
const oversizedCode = structuredClone(codeCandidates); oversizedCode.variants[0].content_alternatives[0].content = 'a'.repeat(256 * 1024 + 1);
assert.match(t.parseTemplateRules([oversizedCode]).error, /256 KiB/);
const tooManyCode = structuredClone(codeCandidates); tooManyCode.variants[0].content_alternatives = Array.from({length: 32}, (_, i) => ({file_path: `pages/${i}.html`, content: '<html>x</html>'}));
assert.match(t.parseTemplateRules([tooManyCode]).error, /32 份/);
const messageCandidates = valid();
messageCandidates.variants[0].delivery = {...t.newTemplateDelivery(), messages: [{at_ms: 1000, text: 'first', alternatives: ['second', 'third'], selected_index: 2}]};
assert.ok(t.parseTemplateRules([messageCandidates]).rules, 'message candidates support explicit selection');
const randomMessages = structuredClone(messageCandidates); randomMessages.variants[0].delivery.messages[0].selected_index = undefined;
assert.ok(t.parseTemplateRules([randomMessages]).rules, 'message candidates support random selection');
for (const bad of [3, -1, 1.5, NaN]) {
  const d = structuredClone(messageCandidates); d.variants[0].delivery.messages[0].selected_index = bad;
  assert.ok(t.parseTemplateRules([d]).error, `invalid message candidate selection ${bad}`);
}
const tooManyMessages = structuredClone(messageCandidates); tooManyMessages.variants[0].delivery.messages[0].alternatives = Array.from({length: 16}, (_, i) => `alternative-${i}`);
assert.match(t.parseTemplateRules([tooManyMessages]).error, /16 条/);
const legacyCandidates = structuredClone(t.parseTemplateRules([messageCandidates]).rules);
delete legacyCandidates[0].variants[0].content_alternatives;
delete legacyCandidates[0].variants[0].content_alternative_index;
delete legacyCandidates[0].variants[0].delivery.messages[0].alternatives;
delete legacyCandidates[0].variants[0].delivery.messages[0].selected_index;
assert.equal(t.templateDrafts(legacyCandidates)[0].variants[0].content_alternatives.length, 0);
assert.deepEqual(plain(t.templateDrafts(legacyCandidates)[0].variants[0].delivery.messages[0].alternatives), []);
assert(t.sameTemplateRules(t.parseTemplateRules(t.templateDrafts(legacyCandidates)).rules, legacyCandidates), 'legacy candidates omitted fields remain semantically equal');
console.log('PASS: multiple code/message candidates, bounds, random selection, and legacy normalization');
