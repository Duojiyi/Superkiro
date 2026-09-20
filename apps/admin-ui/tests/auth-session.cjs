// Offline session-boundary races. No network, browser, or production access.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require('typescript');
function client(fetch) {
  const exports = {};
  const source = fs.readFileSync(path.join(__dirname, '../src/api.ts'), 'utf8');
  vm.runInNewContext(ts.transpileModule(source, {compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022}}).outputText,
    {exports,Headers,AbortController,fetch,setTimeout,clearTimeout});
  return new exports.AdminApiClient();
}
const deferred = () => {let resolve; const promise = new Promise(r => {resolve=r;}); return {promise,resolve};};
const ok = data => ({ok:true,json:async()=>data});
const session = () => ok({success:true,role:'admin',csrfToken:'fixture-csrf'});
(async()=>{
  let calls=0;
  const anonymous=client(async()=>{calls++;return session();});
  for(const operation of [()=>anonymous.getStats(),()=>anonymous.getCards(),()=>anonymous.exportLedger('csv'),()=>anonymous.revealCard('secret')])
    await assert.rejects(operation(),/请先登录/);
  assert.equal(calls,0,'anonymous API calls must not reach fetch');
  for(const invalid of [{success:false},{success:true,role:'user',csrfToken:'token'},{success:true,role:'admin',csrfToken:''}]){
    const api=client(async()=>ok(invalid));await assert.rejects(api.checkAuth(),/无效/);await assert.rejects(api.getStats(),/请先登录/);
  }
  const deniedLogin=client(async()=>ok({success:false}));
  await assert.rejects(deniedLogin.establishSession('admin','invalid'),/登录失败/);
  await assert.rejects(deniedLogin.getStats(),/请先登录/);
  // Invalidate while a successful response body is still being consumed.
  for(const blob of [false,true]){
    const body=deferred(),entered=deferred();let signal;
    const api=client(async(url,options)=>{
      if(url.endsWith('/session'))return session();
      signal=options.signal;return {ok:true,json:()=>{entered.resolve();return body.promise;},blob:()=>{entered.resolve();return body.promise;}};
    });
    await api.checkAuth();const request=blob?api.exportLedger('csv'):api.getStats();const rejected=assert.rejects(request,/会话已改变/);
    await entered.promise;api.clearSession();assert.equal(signal.aborted,true);body.resolve({secret:'old-session'});await rejected;
  }
  // A late 401 from an old session must not log out a new authenticated session.
  const old=deferred();let invalidations=0;
  const api=client(async url=>url.endsWith('/session')?session():old.promise);
  api.onUnauthorized=()=>invalidations++;
  await api.checkAuth();const stale=assert.rejects(api.getStats(),/会话已改变/);
  api.clearSession();await api.checkAuth();old.resolve({ok:false,status:401,json:async()=>({error:'expired'})});
  await stale;assert.equal(invalidations,0);
  // Multiple concurrent old-session 401s cannot invalidate a fresh login.
  const lateResponses=[deferred(),deferred()];let requestIndex=0,concurrentInvalidations=0;
  const concurrent=client(async url=>url.endsWith('/session')?session():lateResponses[requestIndex++].promise);
  concurrent.onUnauthorized=()=>concurrentInvalidations++;
  await concurrent.checkAuth();
  const oldRequests=[assert.rejects(concurrent.getStats(),/会话已改变/),assert.rejects(concurrent.getCards(),/会话已改变/)];
  await concurrent.establishSession('admin','new-password');
  for(const response of lateResponses)response.resolve({ok:false,status:401,json:async()=>({error:'old-session'})});
  await Promise.all(oldRequests);assert.equal(concurrentInvalidations,0);
  lateResponses.push({promise:Promise.resolve(ok({success:true}))});await concurrent.getStats();
  // GET session already in flight at logout cannot restore the CSRF token.
  for(const delayedBody of [false,true]){
    const lateSession=deferred(),entered=deferred();let sessionCalls=0;
    const checking=client(async url=>{
      if(!url.endsWith('/session'))return {ok:true};
      if(++sessionCalls===1)return session();
      if(delayedBody)return {ok:true,json:()=>{entered.resolve();return lateSession.promise;}};
      entered.resolve();return lateSession.promise;
    });
    await checking.checkAuth();const lateCheck=assert.rejects(checking.checkAuth(),/会话已改变/);
    await entered.promise;await checking.logout();
    lateSession.resolve(delayedBody?{success:true,role:'admin',csrfToken:'stale-csrf'}:session());
    await lateCheck;await assert.rejects(checking.getStats(),/请先登录/);
  }
  // A current 401 invalidates immediately, without waiting for the error body.
  const errorBody=deferred(),arrived=deferred();
  const expired=client(async url=>url.endsWith('/session')?session():({ok:false,status:401,json:()=>{arrived.resolve();return errorBody.promise;}}));
  expired.onUnauthorized=()=>invalidations++;
  await expired.checkAuth();const failure=assert.rejects(expired.getStats(),/expired/);await arrived.promise;
  assert.equal(invalidations,1);await assert.rejects(expired.getCards(),/请先登录/);errorBody.resolve({error:'expired'});await failure;
  // Logout locks local access before server completion, even if revocation fails.
  const revoke=deferred();let revocationHeaders;
  const logout=client(async(url,options)=>{if(url.endsWith('/session'))return session();revocationHeaders=options.headers;return revoke.promise;});
  await logout.checkAuth();const done=assert.rejects(logout.logout(),/撤销失败/);
  await assert.rejects(logout.getStats(),/请先登录/);assert.equal(revocationHeaders.get('x-csrf-token'),'fixture-csrf');
  revoke.resolve({ok:false,status:503});await done;await assert.rejects(logout.getStats(),/请先登录/);
  // A shared cookie can change without this tab explicitly logging out.
  // Changed CSRF aborts old work; unchanged CSRF retains in-flight work and drafts.
  for(const kind of ['json', 'blob', 'error']) {
    const delayed=deferred(),entered=deferred(); let token='first',signal,changes=0;
    const shared=client(async(url,options)=>{
      if(url.endsWith('/session'))return ok({success:true,role:'admin',csrfToken:token});
      signal=options.signal;
      return {ok:kind!=='error',status:503,json:()=>{entered.resolve();return delayed.promise;},blob:()=>{entered.resolve();return delayed.promise;}};
    });
    shared.onSessionChanged=()=>changes++;
    await shared.checkAuth();shared.authenticatedUsername='admin';
    const request=kind==='blob'?shared.exportLedger('csv'):shared.revealCard('private-card');
    const rejected=assert.rejects(request,/会话已改变/);
    await entered.promise;await shared.checkAuth();assert.equal(signal.aborted,false);assert.equal(changes,0);
    token='second';await shared.checkAuth();
    assert.equal(signal.aborted,true);assert.equal(changes,1);assert.equal(shared.authenticatedUsername,null);
    delayed.resolve(kind==='error'?{error:'old private error'}:{rawCode:'old private code'});await rejected;
    await shared.checkAuth();assert.equal(changes,1);
    shared.clearSession();
  }
  console.log('PASS: shared-cookie rotation, anonymous request gate, invalid sessions, JSON/blob response races, old/current 401 isolation, immediate logout and revoke failure');
})().catch(error=>{console.error(error);process.exitCode=1;});
