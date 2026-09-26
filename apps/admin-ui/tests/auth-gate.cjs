// Final-build auth gate regression: localhost fixture only, no deployed services.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
const http=require('node:http'),fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict');
const fixture=require('./fixture-api.cjs')();
// The console confirms in its own dialog (role alertdialog), never window.confirm.
async function confirmIn(page,accept){
  const box=page.getByRole('alertdialog');await box.waitFor();
  await (accept?box.locator('[data-confirm="accept"]'):box.getByRole('button',{name:'取消',exact:true})).click();
  await box.waitFor({state:'detached'});
}
async function waitForRoute(ready){
  const deadline=Date.now()+10000;
  while(!ready()){assert(Date.now()<deadline,'timed out waiting for intercepted request');await new Promise(r=>setTimeout(r,10));}
}
const root=path.resolve(__dirname,'../dist');
// Keep generated test artifacts outside the workspace.
const screenshots=fs.mkdtempSync(path.join(require('node:os').tmpdir(),'admin-auth-gate-'));
const server=http.createServer(async(req,res)=>{
  try{
    if(req.url.startsWith('/api/'))return await fixture.handle(req,res);
    const file=path.resolve(root,decodeURIComponent(req.url.split('?')[0]).replace(/^\/admin\/?/,'')||'index.html');
    if(!file.startsWith(root+path.sep)||!fs.existsSync(file)){res.writeHead(404);return res.end();}
    res.setHeader('Content-Type',file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':'text/html');res.end(fs.readFileSync(file));
  }catch(error){res.writeHead(500);res.end(JSON.stringify({error:error.message}));}
});
(async()=>{
  await new Promise(r=>server.listen(0,'127.0.0.1',r));
  const browser=await chromium.launch({headless:true,...(process.env.CHROME_PATH?{executablePath:process.env.CHROME_PATH}:{})});
  try{
    const page=await browser.newPage({viewport:{width:1440,height:1080}});page.setDefaultTimeout(15000);
    const origin=`http://127.0.0.1:${server.address().port}`;
    const requests=[],errors=[];let initial=true,heldSession,holdLoginSession=false,heldRevoke;
    page.on('pageerror',e=>errors.push(e.message));
    const nativeDialogs=[];page.on('dialog',d=>{nativeDialogs.push(d.message());void d.dismiss();});
    await page.route('**/*',async route=>{
      const request=route.request(),url=new URL(request.url());
      if(url.origin!==origin)return route.abort();
      if(url.pathname.startsWith('/api/'))requests.push(url.pathname);
      if(url.pathname.endsWith('/session')&&request.method()==='GET'&&(initial||holdLoginSession)){
        heldSession=route;return;
      }
      if(url.pathname.endsWith('/session/revoke')){heldRevoke=route;return;}
      return route.continue();
    });
    await page.goto(origin+'/admin/');
    await page.getByRole('status').filter({hasText:'正在检查会话'}).waitFor();
    assert.equal(await page.locator('.sidebar,.workspace,input[type=password]').count(),0);
    assert(requests.every(p=>p==='/api/v1/admin/session'));
    await page.waitForFunction(()=>document.querySelector('[role=status]'));
    await waitForRoute(()=>heldSession);
    initial=false;await heldSession.continue();heldSession=null;
    await page.getByRole('heading',{name:'管理员登录',exact:true}).waitFor();
    await page.screenshot({path:path.join(screenshots,'login-unauthenticated.png'),fullPage:true,animations:'disabled'});
    for(const selector of ['.auth-card h1','.auth-card label','.auth-card input'])
      assert.equal(await page.locator(selector).first().evaluate(el=>getComputedStyle(el).color),'rgb(31, 35, 40)');
    await page.keyboard.press('Escape');assert.equal(await page.getByRole('button',{name:'取消',exact:true}).count(),0);
    assert.equal(await page.locator('.sidebar,.workspace,[role=dialog]').count(),0);
    assert(requests.every(p=>p==='/api/v1/admin/session'));
    // The default server does not require 2FA; password-only login remains covered below.
    assert.equal(await page.getByLabel('动态验证码',{exact:true}).count(),0);
    // Rejected credentials produce one error, never a background workspace.
    await page.route('**/api/v1/admin/session',async route=>{
      if(route.request().method()==='POST')return route.fulfill({status:401,contentType:'application/json',body:JSON.stringify({error:'用户名或密码错误'})});
      return route.fallback();
    });
    await page.getByLabel('密码',{exact:true}).fill('bad-password');await page.getByRole('button',{name:'登录',exact:true}).click();
    await page.getByRole('alert').waitFor();assert.equal(await page.getByRole('alert').count(),1);assert.equal(await page.getByLabel('密码',{exact:true}).inputValue(),'');
    assert.equal(await page.locator('.workspace').count(),0);await page.unroute('**/api/v1/admin/session');
    // POST login alone is insufficient: wait for the validated GET session.
    holdLoginSession=true;await page.getByLabel('密码',{exact:true}).fill('fixture-password');await page.getByRole('button',{name:'登录',exact:true}).click();
    await waitForRoute(()=>heldSession);
    assert.equal(await page.locator('.workspace').count(),0);assert(requests.every(p=>p==='/api/v1/admin/session'));
    holdLoginSession=false;await heldSession.continue();heldSession=null;
    await page.getByRole('navigation',{name:'管理导航'}).waitFor();
    await page.getByRole('heading',{name:'运营概览',level:2,exact:true}).waitFor();
    await page.getByRole('button',{name:'刷新',exact:true}).waitFor();
    await page.screenshot({path:path.join(screenshots,'workspace-authenticated.png'),fullPage:true,animations:'disabled'});
    await page.getByRole('navigation').getByRole('button',{name:'财务对账',exact:true}).click();
    await page.getByRole('heading',{name:'结算参数'}).waitFor();
    // Incomplete cost coverage: no margin is computed and unpriced requests are not free.
    await page.getByText('1 次请求未定价，毛利暂不计算',{exact:true}).waitFor();
    await page.getByLabel('积分面值',{exact:true}).fill('0.02');await page.getByLabel('美元汇率',{exact:true}).fill('7.3');await page.getByLabel('变更原因',{exact:true}).fill('fixture settings');
    await page.getByRole('button',{name:'发布',exact:true}).click();
    const financeBox=page.getByRole('alertdialog');await financeBox.waitFor();
    assert((await financeBox.innerText()).includes('0.01 → 0.02'));assert((await financeBox.innerText()).includes('7.2 → 7.3'));
    await confirmIn(page,true);
    await page.getByRole('status').filter({hasText:'已发布结算参数'}).waitFor();
    assert.deepEqual(fixture.writes.find(w=>w.endpoint==='commercial-config').body,{settings:{credit_face_value_cny:0.02,usd_cny_rate:7.3},expected_revision:'fixture-rev-2',reason:'fixture settings'});
    await page.screenshot({path:path.join(screenshots,'financial-settings-estimates.png'),fullPage:true});
    // A lost adjustment response keeps one intent key and immutable parameters.
    await page.getByRole('navigation').getByRole('button',{name:'卡密资产',exact:true}).click();
    const adjustmentBodies=[];
    await page.route('**/api/v1/admin/cards/adjust',async route=>{
      adjustmentBodies.push(route.request().postDataJSON());
      return route.fulfill({status:adjustmentBodies.length===1?503:adjustmentBodies.length===2?400:200,contentType:'application/json',body:JSON.stringify(adjustmentBodies.length===1?{error:'fixture lost response'}:adjustmentBodies.length===2?{error:'unclassified rejection'}:{success:true})});
    });
    await page.getByRole('button',{name:'调账',exact:true}).first().click();
    await page.getByRole('textbox',{name:'调账原因说明'}).fill('fixture adjustment');
    for(const amount of ['1000001','0.0000001','-0.0000001','1.0000001']){
      await page.getByRole('spinbutton',{name:'增减积分数量'}).fill(amount);
      assert(await page.getByRole('button',{name:'下一步',exact:true}).isDisabled(),`${amount} must not reach review`);
      assert.equal(adjustmentBodies.length,0);assert.equal(await page.evaluate(()=>sessionStorage.length),0);
      assert.equal(await page.getByRole('spinbutton',{name:'增减积分数量'}).isDisabled(),false);
    }
    await page.getByRole('spinbutton',{name:'增减积分数量'}).fill('10');
    // The review step names card, before → after and the reason before anything is sent.
    await page.getByRole('button',{name:'下一步',exact:true}).click();
    assert((await page.getByLabel('调账复核').innerText()).includes('fixture adjustment'));assert.equal(adjustmentBodies.length,0);
    await page.getByRole('button',{name:'确认入账',exact:true}).click();
    await page.getByRole('dialog').getByRole('alert').waitFor();
    assert(await page.getByRole('spinbutton',{name:'增减积分数量'}).isDisabled());
    // The server names the operator, so a reload restores the same intent without a new sign-in.
    await page.reload();await page.getByRole('navigation',{name:'管理导航'}).waitFor();
    assert.equal(adjustmentBodies.length,1);
    await page.getByRole('navigation').getByRole('button',{name:'卡密资产',exact:true}).click();
    await page.getByRole('button',{name:'调账',exact:true}).first().click();
    assert.equal(await page.getByRole('textbox',{name:'调账原因说明'}).inputValue(),'fixture adjustment');
    assert(await page.getByRole('spinbutton',{name:'增减积分数量'}).isDisabled(),'a pending intent keeps its original parameters');
    await page.screenshot({path:path.join(screenshots,'adjustment-restored.png'),fullPage:true});
    assert.equal(adjustmentBodies.length,1);
    await page.getByRole('button',{name:'下一步',exact:true}).click();await page.getByRole('button',{name:'确认入账',exact:true}).click();
    await page.getByRole('dialog').getByRole('alert').filter({hasText:'unclassified rejection'}).waitFor();
    assert.equal(await page.evaluate(()=>sessionStorage.length),1);assert(await page.getByRole('spinbutton',{name:'增减积分数量'}).isDisabled());
    await page.getByRole('button',{name:'确认入账',exact:true}).click();
    await page.getByRole('dialog').waitFor({state:'detached'});
    assert.deepEqual(adjustmentBodies[1],adjustmentBodies[2]);
    assert.equal(adjustmentBodies.length,3);assert(adjustmentBodies[0].idempotencyKey);assert.deepEqual(adjustmentBodies[0],adjustmentBodies[1]);

    // Old zero-micro intent can only be discarded explicitly; no recovery POST.
    await page.evaluate(()=>sessionStorage.setItem('superkiro.pending-adjustment.v1:admin',JSON.stringify({operator:'admin',cardId:'fixture-card-0',delta:0.0000001,reason:'legacy zero micro',key:'legacy-zero-micro'})));
    await page.getByRole('button',{name:'调账',exact:true}).first().click();
    await page.getByRole('button',{name:'清除零微积分意图'}).waitFor();
    await page.getByRole('button',{name:'清除零微积分意图'}).click();await confirmIn(page,false);assert.equal(await page.evaluate(()=>sessionStorage.length),1);
    await page.getByRole('button',{name:'清除零微积分意图'}).click();await confirmIn(page,true);assert.equal(await page.evaluate(()=>sessionStorage.length),0);assert.equal(adjustmentBodies.length,3);
    assert.equal(await page.getByRole('spinbutton',{name:'增减积分数量'}).isDisabled(),false);
    await page.getByRole('button',{name:'取消',exact:true}).click();
    await page.getByRole('navigation').getByRole('button',{name:'模型与定价',exact:true}).click();
    await page.getByText('编辑 JSON（高级）',{exact:true}).click();
    const draft=page.locator('textarea[aria-label="配置 JSON"]');await draft.fill('{"models":[],"privateDraft":"old-sensitive-draft"}');
    holdLoginSession=true;await page.evaluate(()=>window.dispatchEvent(new Event('focus')));await waitForRoute(()=>heldSession);
    assert.equal(await draft.inputValue(),'{"models":[],"privateDraft":"old-sensitive-draft"}');
    // A re-check in progress no longer covers the workspace; writes still need the session and CSRF.
    assert.equal(await draft.evaluate(el=>!!el.closest('[inert]')),false);
    assert.equal(await page.getByRole('status').filter({hasText:'正在检查会话'}).count(),0);
    holdLoginSession=false;await heldSession.continue();heldSession=null;
    assert.equal(await draft.inputValue(),'{"models":[],"privateDraft":"old-sensitive-draft"}');
    // A re-check that cannot complete blocks the workspace, keeping the draft, until it succeeds.
    holdLoginSession=true;await page.evaluate(()=>window.dispatchEvent(new Event('focus')));await waitForRoute(()=>heldSession);
    await heldSession.fulfill({status:503,contentType:'application/json',body:JSON.stringify({error:'temporary outage'})});heldSession=null;holdLoginSession=false;
    const recheck=page.getByRole('alertdialog',{name:'无法确认登录状态'});
    await recheck.getByRole('button',{name:'重试'}).waitFor();assert.equal(await draft.inputValue(),'{"models":[],"privateDraft":"old-sensitive-draft"}');assert(await draft.evaluate(el=>!!el.closest('[inert]')));
    await page.screenshot({path:path.join(screenshots,'session-recheck-blocked.png'),fullPage:true});
    await recheck.getByRole('button',{name:'重试'}).click();await recheck.waitFor({state:'detached'});
    assert.equal(await draft.evaluate(el=>!!el.closest('[inert]')),false);
    await page.getByRole('navigation').getByRole('button',{name:'分组与权益',exact:true}).click();await confirmIn(page,false);assert.equal(await draft.inputValue(),'{"models":[],"privateDraft":"old-sensitive-draft"}');
    fixture.expire();await page.getByRole('button',{name:'刷新',exact:true}).click();
    await page.getByRole('heading',{name:'管理员登录',exact:true}).waitFor();
    assert.equal(await page.locator('.workspace,.sidebar,[role=dialog],textarea').count(),0);
    assert.equal(await page.getByText('old-sensitive-draft',{exact:false}).count(),0);
    await page.getByLabel('密码',{exact:true}).fill('fixture-password');
    // The typed password lives in the input's value property only, never in markup.
    assert(!(await page.content()).includes('fixture-password'),'password mirrored into the DOM');
    await page.getByRole('button',{name:'登录',exact:true}).click();
    await page.getByRole('heading',{name:'运营概览',level:2,exact:true}).waitFor();
    await page.getByRole('navigation').getByRole('button',{name:'模型与定价',exact:true}).click();
    await page.getByText('编辑 JSON（高级）',{exact:true}).click();
    // An unpublished draft (no secrets) survives the expired session in this tab and is
    // restored after the next login onto the configuration it was made from, never before.
    await page.getByRole('status').filter({hasText:'已恢复未发布的修改'}).waitFor();
    assert((await page.getByRole('textbox',{name:'配置 JSON',exact:true}).inputValue()).includes('old-sensitive-draft'));
    // Focus revalidation must hide the old workspace and reject revoked cookies.
    fixture.expire();await page.evaluate(()=>window.dispatchEvent(new Event('focus')));
    await page.getByRole('heading',{name:'管理员登录',exact:true}).waitFor();assert.equal(await page.locator('.workspace,textarea').count(),0);
    await page.getByLabel('密码',{exact:true}).fill('fixture-password');await page.getByRole('button',{name:'登录',exact:true}).click();
    await page.getByRole('heading',{name:'运营概览',level:2,exact:true}).waitFor();
    // Absolute expiry must remove the workspace even without further user activity.
    await page.route('**/api/v1/admin/session',async route=>{
      if(route.request().method()==='GET')return route.fulfill({status:200,contentType:'application/json',body:JSON.stringify({success:true,role:'admin',csrfToken:'fixture-csrf',expiresAt:Date.now()/1000+2,twoFactorEnabled:false,totpRequired:false})});
      return route.fallback();
    });
    await page.evaluate(()=>window.dispatchEvent(new Event('focus')));
    await page.getByRole('navigation',{name:'管理导航'}).waitFor();
    await page.getByRole('heading',{name:'管理员登录',exact:true}).waitFor();assert.equal(await page.locator('.workspace,.sidebar,textarea').count(),0);
    await page.unroute('**/api/v1/admin/session');
    await page.getByLabel('密码',{exact:true}).fill('fixture-password');await page.getByRole('button',{name:'登录',exact:true}).click();
    await page.getByRole('heading',{name:'运营概览',level:2,exact:true}).waitFor();
    await page.route('**/api/v1/admin/session',async route=>{
      if(route.request().method()!=='GET')return route.fallback();
      const response=await route.fetch(),data=await response.json();delete data.username;
      return route.fulfill({response,json:data});
    });
    await page.reload();await page.getByRole('navigation',{name:'管理导航'}).waitFor();
    await page.getByRole('navigation').getByRole('button',{name:'卡密资产',exact:true}).click();
    await page.getByRole('button',{name:'调账',exact:true}).first().click();
    await page.getByRole('alert').filter({hasText:'需要重新登录以确认操作人'}).getByRole('button',{name:'重新登录'}).waitFor();
    assert.equal(await page.getByRole('dialog',{name:'卡密调账'}).count(),0);assert.equal(adjustmentBodies.length,3);
    await page.unroute('**/api/v1/admin/session');await page.reload();await page.getByRole('navigation',{name:'管理导航'}).waitFor();
    await page.getByRole('button',{name:'退出',exact:true}).click();
    await page.getByRole('heading',{name:'管理员登录',exact:true}).waitFor();assert.equal(await page.locator('.workspace,.sidebar,textarea').count(),0);
    await waitForRoute(()=>heldRevoke);
    const count=requests.length;await heldRevoke.fulfill({status:503,contentType:'application/json',body:'{}'});
    await page.getByRole('alert').waitFor();assert.equal(requests.length,count);assert.equal(await page.locator('.workspace,.sidebar').count(),0);
    assert.equal(await page.evaluate(()=>localStorage.length+sessionStorage.length),0);
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    // Separate anonymous context: the server explicitly requires TOTP.
    const twoFactorPage=await browser.newPage();
    try {
      const loginBodies=[],twoFactorRequests=[];
      twoFactorPage.on('pageerror',e=>errors.push(e.message));
      await twoFactorPage.route('**/*',route=>{
        const request=route.request(),url=new URL(request.url());
        if(url.origin!==origin)return route.abort();
        if(!url.pathname.startsWith('/api/'))return route.continue();
        twoFactorRequests.push(url.pathname);
        assert.equal(url.pathname,'/api/v1/admin/session','2FA must not allow business-data requests before authentication');
        if(request.method()==='POST')loginBodies.push(request.postDataJSON());
        return route.fulfill({status:401,json:{error:request.method()==='POST'?'验证码错误':'需要管理员登录',twoFactorEnabled:true,totpRequired:request.method()==='POST'}});
      });
      await twoFactorPage.goto(origin+'/admin/');
      const code=twoFactorPage.getByLabel('动态验证码',{exact:true});
      await twoFactorPage.getByRole('heading',{name:'管理员登录',exact:true}).waitFor();
      assert.equal(await code.count(),0);
      await twoFactorPage.getByLabel('密码',{exact:true}).fill('fixture-password');
      await twoFactorPage.getByRole('button',{name:'登录',exact:true}).click();
      await code.waitFor();
      assert.deepEqual(loginBodies,[{username:'admin',password:'fixture-password'}]);assert(await code.evaluate(input=>input.required));
      await twoFactorPage.getByLabel('密码',{exact:true}).fill('fixture-password');
      for(const invalid of ['', '12', 'abcdef']) {
        await code.fill(invalid);
        const before=twoFactorRequests.length;
        await twoFactorPage.getByRole('button',{name:'登录',exact:true}).click();
        assert.equal(await code.evaluate(input=>input.checkValidity()),false);
        assert.equal(twoFactorRequests.length,before);assert.equal(loginBodies.length,1);
      }
      await code.fill('123456');assert(await code.evaluate(input=>input.checkValidity()));
      await twoFactorPage.getByRole('button',{name:'登录',exact:true}).click();
      await waitForRoute(()=>loginBodies.length===2);
      await twoFactorPage.getByRole('alert').filter({hasText:'验证码错误'}).waitFor();
      assert.deepEqual(loginBodies,[{username:'admin',password:'fixture-password'},{username:'admin',password:'fixture-password',totpCode:'123456'}]);
      assert.equal(await twoFactorPage.getByRole('alert').count(),1);
      assert.equal(await twoFactorPage.getByLabel('密码',{exact:true}).inputValue(),'');
      assert.equal(await code.inputValue(),'');assert(await code.evaluate(input=>input.required));
      assert.equal(await twoFactorPage.locator('.workspace,.sidebar').count(),0);
      assert.equal(await twoFactorPage.evaluate(()=>localStorage.length+sessionStorage.length),0);
    } finally {await twoFactorPage.close();}
    assert.deepEqual(errors,[]);
    fs.writeFileSync(path.join(screenshots,'results.json'),JSON.stringify({generatedAt:new Date().toISOString(),scope:'final built UI, localhost fixture only',precisionRegression:true,authGatePassed:true},null,2));
    console.log('PASS: stable adjustment retries, default password-only login, server-required TOTP validation/payload/rejection, focus reauthentication, idle expiry, checking/login gates, no pre-auth sensitive requests, no cancel bypass, single login error, session GET barrier, 401 editor teardown, fresh re-login, immediate logout and revoke failure');
  }finally{await browser.close();}
})().catch(e=>{console.error(e);process.exitCode=1;}).finally(()=>server.close());
