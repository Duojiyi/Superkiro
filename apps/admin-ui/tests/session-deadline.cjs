// Offline timers and transport fixture; no live administrator session.
const assert=require('node:assert/strict'),fs=require('node:fs'),vm=require('node:vm'),ts=require('typescript');
const source=ts.transpileModule(fs.readFileSync(require('node:path').join(__dirname,'../src/api.ts'),'utf8'),{compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022}}).outputText;
const timers=new Map();let next=0,body,hang=false,expiredOnServer=false,deadline=null,lastHeaders=null;const backgroundChecks=[];
const exportsFixture={};
const reply=value=>({ok:true,status:200,headers:new Headers(deadline?{'x-admin-session-expires':String(deadline)}:{}),json:async()=>value});
vm.runInNewContext(source,{exports:exportsFixture,Headers,AbortController,Date,
 setTimeout:(fn,ms)=>{timers.set(++next,{fn,ms});return next;},clearTimeout:id=>timers.delete(id),
 fetch:async(url,options)=>{
  if(hang)return new Promise((_,reject)=>options.signal.addEventListener('abort',()=>reject(new Error('aborted'))));
  if(options.body)body=JSON.parse(options.body);lastHeaders=options.headers;
  if(url.endsWith('/session')&&options.headers.get('x-admin-background')==='1')backgroundChecks.push(url);
  if(expiredOnServer)return {ok:false,status:401,headers:new Headers(),json:async()=>({error:'expired'})};
  return reply(url.endsWith('/session')?{success:true,role:'admin',csrfToken:'fixture',expiresAt:Math.floor(Date.now()/1000)+60,twoFactorEnabled:true,totpRequired:true}:{success:true});
 }});
// The session's end is the latest timer; the warning comes two minutes before it.
const expiryTimer=()=>[...timers.values()].filter(t=>t.ms>15000).sort((a,b)=>b.ms-a.ms)[0];
(async()=>{
 const api=new exportsFixture.AdminApiClient();let invalidations=0;const reasons=[];api.onUnauthorized=reason=>{invalidations++;reasons.push(reason);};
 await api.establishSession('admin','password','123456');assert.equal(body.totpCode,'123456');assert.equal(api.twoFactorEnabled,true);
 await api.adjustBalance('card',10,'reason','same-intent');assert.equal(body.idempotencyKey,'same-intent');
 await api.adjustBalance('card',10,'reason','same-intent');assert.equal(body.idempotencyKey,'same-intent');
 // Automatic refreshes say so, so they do not keep an idle session alive; the operator's own calls do not.
 await api.inBackground(()=>api.getStats());assert.equal(lastHeaders.get('x-admin-background'),'1');
 await api.getStats();assert.equal(lastHeaders.get('x-admin-background'),null);
 // Each use moves the idle deadline: the reply names it and the timers follow.
 deadline=Math.floor(Date.now()/1000)+1800;await api.getStats();
 const moved=expiryTimer();assert(moved&&moved.ms>1790000&&moved.ms<=1800000,`deadline follows the reply: ${moved&&moved.ms}`);
 // At the local deadline the server is asked once, in the background (not counted as use);
 // a deadline moved elsewhere keeps the session.
 deadline=null;await moved.fn();assert.equal(invalidations,0);assert.equal(backgroundChecks.length,1);
 // Expiry is still enforced: when the server says the session is over, it ends, with the reason.
 expiredOnServer=true;await expiryTimer().fn();assert.equal(invalidations,1);assert.deepEqual(reasons,['expired']);assert.equal(backgroundChecks.length,2);
 await assert.rejects(api.getStats(),/请先登录/);
 expiredOnServer=false;await api.checkAuth();hang=true;const pending=assert.rejects(api.getStats(),/请求超时/);
 [...timers.values()].find(t=>t.ms===15000).fn();await pending;
 api.clearSession();console.log('PASS deadline, sliding idle deadline with a background re-check, idle expiry, TOTP payload and adjustment key contracts');
})().catch(error=>{console.error(error);process.exitCode=1;});
