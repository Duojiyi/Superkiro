// Offline timers and transport fixture; the operator's clock is 20 minutes ahead of the server.
const assert=require('node:assert/strict'),fs=require('node:fs'),vm=require('node:vm'),ts=require('typescript');
const source=ts.transpileModule(fs.readFileSync(require('node:path').join(__dirname,'../src/api.ts'),'utf8'),{compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022}}).outputText;
const SKEW=20*60*1000,LIFETIME=900;
class OperatorClock extends Date{static now(){return Date.now()+SKEW;}}
const timers=new Map();let next=0;
const exportsFixture={};
vm.runInNewContext(source,{exports:exportsFixture,Headers,AbortController,Date:OperatorClock,
 setTimeout:(fn,ms)=>{timers.set(++next,{fn,ms});return next;},clearTimeout:id=>timers.delete(id),
 // The server's own clock and lifetime, exactly as admin_login.rs reports them.
 fetch:async url=>({ok:true,json:async()=>url.endsWith('/session')?{success:true,role:'admin',csrfToken:'fixture',
   expiresAt:Math.floor(Date.now()/1000)+LIFETIME,expiresIn:LIFETIME,twoFactorEnabled:false,totpRequired:false}:{success:true}})});
(async()=>{
 const api=new exportsFixture.AdminApiClient();const ends=[];let warnings=0;
 api.onUnauthorized=reason=>ends.push(reason);api.onExpiring=()=>warnings++;
 // A clock ahead of the server must not end a brand-new session.
 await api.establishSession('admin','password');
 assert.deepEqual(ends,[]);
 const scheduled=[...timers.values()].map(t=>t.ms).sort((a,b)=>a-b);
 assert(scheduled.includes(LIFETIME*1000),`session ends after its lifetime: ${scheduled}`);
 assert(scheduled.includes(LIFETIME*1000-exportsFixture.SESSION_WARNING_MS),`warning before the end: ${scheduled}`);
 [...timers.values()].find(t=>t.ms===LIFETIME*1000-exportsFixture.SESSION_WARNING_MS).fn();
 assert.equal(warnings,1);
 // The end says why, so the login page can tell the operator.
 [...timers.values()].find(t=>t.ms===LIFETIME*1000).fn();
 assert.deepEqual(ends,['expired']);
 await assert.rejects(api.getStats(),/请先登录/);
 console.log('PASS server-relative session lifetime under clock skew, expiry warning and expiry reason');
})().catch(error=>{console.error(error);process.exitCode=1;});
