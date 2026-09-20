// Offline timers and transport fixture; no live administrator session.
const assert=require('node:assert/strict'),fs=require('node:fs'),vm=require('node:vm'),ts=require('typescript');
const source=ts.transpileModule(fs.readFileSync(require('node:path').join(__dirname,'../src/api.ts'),'utf8'),{compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022}}).outputText;
const timers=new Map();let next=0,body,hang=false;
const exportsFixture={};
vm.runInNewContext(source,{exports:exportsFixture,Headers,AbortController,Date,
 setTimeout:(fn,ms)=>{timers.set(++next,{fn,ms});return next;},clearTimeout:id=>timers.delete(id),
 fetch:async(url,options)=>{
  if(hang)return new Promise((_,reject)=>options.signal.addEventListener('abort',()=>reject(new Error('aborted'))));
  if(options.body)body=JSON.parse(options.body);
  return {ok:true,json:async()=>url.endsWith('/session')?{success:true,role:'admin',csrfToken:'fixture',expiresAt:Math.floor(Date.now()/1000)+60,twoFactorEnabled:true,totpRequired:true}:{success:true}};
 }});
(async()=>{
 const api=new exportsFixture.AdminApiClient();let invalidations=0;api.onUnauthorized=()=>invalidations++;
 await api.establishSession('admin','password','123456');assert.equal(body.totpCode,'123456');assert.equal(api.twoFactorEnabled,true);
 await api.adjustBalance('card',10,'reason','same-intent');assert.equal(body.idempotencyKey,'same-intent');
 await api.adjustBalance('card',10,'reason','same-intent');assert.equal(body.idempotencyKey,'same-intent');
 const expiry=[...timers.values()].find(t=>t.ms>15000);assert(expiry);expiry.fn();assert.equal(invalidations,1);
 await assert.rejects(api.getStats(),/请先登录/);
 await api.checkAuth();hang=true;const pending=assert.rejects(api.getStats(),/请求超时/);
 [...timers.values()].find(t=>t.ms===15000).fn();await pending;
 api.clearSession();console.log('PASS deadline, idle expiry, TOTP payload and adjustment key contracts');
})().catch(error=>{console.error(error);process.exitCode=1;});
