// Final-build auth gate regression: localhost fixture only, no deployed services.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
const http=require('node:http'),fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict');
const fixture=require('./fixture-api.cjs')();
async function waitForRoute(ready){
  const deadline=Date.now()+10000;
  while(!ready()){assert(Date.now()<deadline,'timed out waiting for intercepted request');await new Promise(r=>setTimeout(r,10));}
}
const root=path.resolve(__dirname,'../dist');

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
  try {
    const page=await browser.newPage({viewport:{width:320,height:740}});
    const origin=`http://127.0.0.1:${server.address().port}`, errors=[];
    page.on('pageerror',e=>errors.push(e.message));
    await page.route('**/*',route=>new URL(route.request().url()).origin===origin?route.continue():route.abort());
    await page.goto(origin+'/admin/');
    await page.getByLabel('密码',{exact:true}).fill('fixture-password');
    await page.getByRole('button',{name:'登录',exact:true}).click();
    const nav=async name=>{await page.getByRole('navigation').getByRole('button',{name,exact:true}).click();};
    await page.getByRole('button',{name:'刷新',exact:true}).waitFor();
    for(const name of ['运营概览','卡密资产','分组与权益','供应商与 Key','模型与定价','调用追踪','财务对账','公告管理','安全与审计']) {
      await nav(name);
      assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),`${name}: 320px overflow`);
    }
    await nav('供应商与 Key');
    let statusRequests=0,held;
    await page.route('**/api/v1/admin/providers/status',route=>{statusRequests++;held=route;});
    const toggle=page.getByRole('button',{name:'停用此上游',exact:true});
    page.once('dialog',dialog=>dialog.dismiss());await toggle.click();assert.equal(statusRequests,0);
    page.once('dialog',dialog=>dialog.accept());await toggle.click();
    await waitForRoute(()=>held);assert(await toggle.isDisabled());assert.equal(statusRequests,1);
    assert.deepEqual(held.request().postDataJSON(),{providerId:'fixture-provider',enabled:false});
    await held.fulfill({status:503,contentType:'application/json',body:'{"error":"fixture unavailable"}'});
    await page.getByRole('alert').filter({hasText:'切换结果未确认'}).waitFor();
    // Provider operations must not report success for a negative acknowledgement.
    await page.getByRole('button',{name:'编辑 →',exact:true}).first().click();
    await page.route('**/api/v1/admin/providers/keys/discover',route=>route.fulfill({status:200,contentType:'application/json',body:JSON.stringify({success:true,models:['fixture-discovered']})}));
    await page.getByRole('button',{name:'获取模型草稿',exact:true}).click();
    await page.getByRole('button',{name:'填入权限草稿',exact:true}).waitFor();
    await page.getByLabel('Key ID',{exact:true}).fill('another-key');
    assert.equal(await page.getByRole('button',{name:'填入权限草稿',exact:true}).count(),0);
    await page.route('**/api/v1/admin/providers/keys',route=>route.fulfill({status:200,contentType:'application/json',body:'{"success":false}'}));
    page.once('dialog',d=>d.accept());
    await page.getByRole('button',{name:'确认保存权限',exact:true}).click();
    await page.getByRole('status').filter({hasText:'服务器未确认操作成功'}).waitFor();
    page.once('dialog',d=>d.dismiss());
    await nav('安全与审计');
    await page.getByLabel('Key ID',{exact:true}).waitFor();
    page.once('dialog',d=>d.accept());
    await nav('安全与审计');
    await page.getByText('未启用：当前仅使用账号和密码登录。',{exact:true}).waitFor();
    page.once('dialog',d=>d.dismiss());
    await page.getByRole('button',{name:'全部会话下线',exact:true}).click();
    assert.equal(fixture.writes.filter(w=>w.endpoint==='session/revoke').length,0);
    await nav('卡密资产');
    await page.route('**/api/v1/admin/cards/status',route=>route.fulfill({status:200,contentType:'application/json',body:'{"success":false}'}));
    page.once('dialog',d=>d.accept());
    await page.getByRole('button',{name:'冻结',exact:true}).first().click();
    await page.getByRole('alert').filter({hasText:'服务器未确认状态变更'}).waitFor();
    await page.getByRole('button',{name:'关闭提示',exact:true}).click();
    const batch=page.getByRole('button',{name:'＋ 批量生成',exact:true});await batch.click();
    let modal=page.getByRole('dialog',{name:'批量生成卡密',exact:true});await modal.waitFor();
    assert(await page.locator('.workspace').evaluate(e=>e.inert));
    assert(await page.locator('.sidebar').evaluate(e=>e.inert));
    await modal.getByRole('button',{name:'取消',exact:true}).focus();
    await page.keyboard.press('Shift+Tab');
    assert(await modal.evaluate(e=>e.contains(document.activeElement)));
    await page.keyboard.press('Escape');assert.equal(await page.getByRole('dialog').count(),0);
    assert(await batch.evaluate(e=>e===document.activeElement));assert.equal(await page.locator('.workspace').evaluate(e=>e.inert),false);
    await page.getByRole('button',{name:'查看卡密',exact:true}).first().click();
    modal=page.getByRole('dialog',{name:'查看卡密',exact:true});await modal.waitFor();
    assert(await modal.evaluate(e=>{const r=e.firstElementChild.getBoundingClientRect();return r.left>=0&&r.right<=innerWidth;}));
    await page.evaluate(()=>Object.defineProperty(navigator,'clipboard',{configurable:true,value:{writeText:async()=>{throw new Error('denied');}}}));
    await modal.getByRole('button',{name:'复制卡密',exact:true}).click();
    await modal.getByRole('alert').filter({hasText:'复制失败'}).waitFor();
    await page.evaluate(()=>Object.defineProperty(navigator,'clipboard',{configurable:true,value:{writeText:async()=>{}}}));
    await modal.getByRole('button',{name:'复制卡密',exact:true}).click();
    await page.getByRole('status').filter({hasText:'已复制卡密'}).waitFor();
    assert.equal(await modal.getByRole('alert').innerText(),'');
    await page.keyboard.press('Escape');assert.equal(await page.getByLabel('卡密明文').count(),0);
    await nav('公告管理');
    await page.getByLabel('公告标题',{exact:true}).fill('fixture');
    await page.getByLabel('正文内容',{exact:true}).fill('fixture notice');
    await page.getByRole('button',{name:'预览并确认发布',exact:true}).click();
    modal=page.getByRole('dialog',{name:'发布服务公告',exact:true});
    await modal.getByLabel('公告标题',{exact:true}).fill('');
    await modal.getByRole('button',{name:'立即发布',exact:true}).click();
    await modal.getByRole('alert').filter({hasText:'不能为空'}).waitFor();
    await modal.getByLabel('公告标题',{exact:true}).fill('测'.repeat(257));
    await modal.getByLabel('正文内容',{exact:true}).fill('fixture notice');
    await modal.getByRole('button',{name:'立即发布',exact:true}).click();
    await modal.getByRole('alert').filter({hasText:'最多 256 字'}).waitFor();
    assert.equal(fixture.writes.filter(w=>w.endpoint==='announcements').length,0);
    // The server may commit while the POST response is lost. Exercise the real 15s timeout.
    let noticePosts=0, noticeReads=0, failNoticeRead=true, heldNotice;
    const published={id:'fixture-uncertain',title:'fixture timeout notice',content:'fixture committed content',level:'info',enabled:true,created_at:Math.floor(Date.now()/1000)};
    await page.route('**/api/v1/admin/announcements',route=>{
      if(route.request().method()==='POST') {
        noticePosts++;heldNotice=route;
        if(noticePosts===1)return; // Commit simulated by the subsequent GET, no response to the POST.
        return route.fulfill({status:200,contentType:'application/json',body:'{"success":false}'});
      }
      noticeReads++;
      return route.fulfill({status:failNoticeRead?503:200,contentType:'application/json',body:JSON.stringify(failNoticeRead?{error:'fixture unavailable'}:{success:true,announcements:[published]})});
    });
    await modal.getByLabel('公告标题',{exact:true}).fill(published.title);
    await modal.getByLabel('正文内容',{exact:true}).fill(published.content);
    page.once('dialog',d=>d.accept());await modal.getByRole('button',{name:'立即发布',exact:true}).click();
    await waitForRoute(()=>heldNotice);assert.equal(noticePosts,1);
    assert(await modal.getByRole('button',{name:'立即发布',exact:true}).isDisabled());
    await page.getByRole('alert').filter({hasText:'公告发布结果未确认'}).waitFor({timeout:25000});
    assert.equal(await page.getByRole('dialog').count(),0);
    assert.equal(await page.getByLabel('公告标题',{exact:true}).inputValue(),published.title);
    assert.equal(await page.getByLabel('正文内容',{exact:true}).inputValue(),published.content);
    const preview=page.getByRole('button',{name:'预览并确认发布',exact:true});
    assert(await preview.isDisabled());
    await page.getByRole('button',{name:'关闭提示',exact:true}).click();
    await nav('运营概览');await nav('公告管理');assert(await preview.isDisabled());
    const unlock=page.getByRole('button',{name:'已核对列表，解除发布限制',exact:true});
    assert(await unlock.isDisabled());
    await page.getByRole('button',{name:'刷新公告以核对',exact:true}).click();
    await page.getByRole('alert').filter({hasText:'公告核对刷新失败'}).waitFor();
    assert(await unlock.isDisabled());assert(await preview.isDisabled());assert.equal(noticePosts,1);
    failNoticeRead=false;
    await page.getByRole('button',{name:'刷新公告以核对',exact:true}).click();
    await page.locator('tbody').getByText(published.title,{exact:true}).waitFor();
    assert(await preview.isDisabled());assert.equal(noticePosts,1);assert.equal(noticeReads,2);
    page.once('dialog',d=>d.dismiss());await unlock.click();assert(await preview.isDisabled());
    page.once('dialog',d=>d.accept());await unlock.click();assert.equal(await preview.isDisabled(),false);assert.equal(noticePosts,1);
    // A 200 response without success is also uncertain, not silent permission to retry.
    await preview.click();page.once('dialog',d=>d.accept());
    await modal.getByRole('button',{name:'立即发布',exact:true}).click();
    await page.getByRole('alert').filter({hasText:'公告发布结果未确认'}).waitFor();
    assert.equal(noticePosts,2);assert(await preview.isDisabled());assert(await unlock.isDisabled());
    console.log('PASS: announcement real timeout after simulated commit; no blind retry; failed refresh stays locked; successful refresh plus explicit review required; success:false locks again');
    await page.reload();await nav('公告管理');
    assert(await page.getByRole('button',{name:'预览并确认发布',exact:true}).isDisabled());
    assert(await page.getByRole('button',{name:'已核对列表，解除发布限制',exact:true}).isDisabled());
    await nav('分组与权益');
    await page.getByText('当前配置已加载。历史价格只读；调整价格请创建新版本。',{exact:true}).waitFor();
    await page.getByLabel('变更原因',{exact:true}).fill('fixture negative acknowledgement');
    await page.route('**/api/v1/admin/commercial-config',route=>route.request().method()==='POST' ? route.fulfill({status:200,contentType:'application/json',body:'{"success":false}'}) : route.continue());
    page.once('dialog',d=>d.accept());
    await page.getByRole('button',{name:'确认并发布',exact:true}).click();
    await page.getByRole('status').filter({hasText:'服务器未确认发布成功'}).waitFor();
    assert.equal(await page.getByLabel('变更原因',{exact:true}).inputValue(),'fixture negative acknowledgement');
    console.log('PASS: stale model discovery discarded; provider/config negative acknowledgement preserves failure; revoke-all cancel sends no request');
    assert.deepEqual(errors,[]);
    console.log('PASS: nine pages at 320px; provider confirmation/pending/error contract; inert dialogs, Escape/focus restore; clipboard failure/retry/live status; announcement validation without writes');
  } finally {await browser.close();}
})().catch(e=>{console.error(e);process.exitCode=1;}).finally(()=>server.close());
