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
    const page=await browser.newPage();
    const origin=`http://127.0.0.1:${server.address().port}`, errors=[], posts=[];
    page.on('pageerror',error=>errors.push(error.message));
    const nativeDialogs=[];page.on('dialog',d=>{nativeDialogs.push(d.message());void d.dismiss();});
    await page.route('**/*',route=>new URL(route.request().url()).origin===origin?route.continue():route.abort());
    // Eligibility comes only from the flag, never IDs or names (including misleading names).
    const legacy={id:'random-a',name:'PRO+',rate_card_id:'fixture-rate'};
    const enabled={id:'random-b',name:'验收测试组',issuance_enabled:true,rate_card_id:'fixture-rate'};
    const blocked={id:'random-c',name:'正式生产组',issuance_enabled:false,rate_card_id:'fixture-rate'};
    let groups=[blocked,legacy], saved;
    await page.route('**/api/v1/admin/commercial-config',async route=>{
      const response=await route.fetch();const data=await response.json();
      await route.fulfill({json:{...data,config:{...data.config,groups}}});
    });
    await page.route('**/api/v1/admin/cards/batch',route=>{
      posts.push(route.request().postDataJSON());
      return route.fulfill({json:{success:true,cards:[{cardId:'issued',rawCode:'TEST-ONLY',groupId:posts.at(-1).groupId,creditTotal:1000000000}]}});
    });
    const button=name=>page.getByRole('button',{name,exact:true});
    const select=()=>page.getByLabel('模型与计费分组',{exact:true});
    const open=async()=>{await button('卡密资产').click();await button('＋ 批量生成').click();};
    const dialog=page.getByRole('dialog',{name:'批量生成卡密'});
    // The generate button reads 生成 N 张; locate it by place so its label can follow the count.
    const generate=()=>dialog.locator('.modal-actions .btn-primary');
    const cancel=()=>dialog.getByRole('button',{name:'取消',exact:true}).click();
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();await open();
    assert.equal(await select().inputValue(),legacy.id);
    assert.deepEqual(await select().locator('option').evaluateAll(nodes=>nodes.map(n=>n.value)),['',legacy.id]);
    for(const tier of ['tier-1000','tier-2000','tier-5000','tier-10000']){
      await page.getByLabel('积分套餐',{exact:true}).selectOption(tier);
      assert.equal(await select().inputValue(),legacy.id);
      assert((await page.getByLabel('发卡摘要').innerText()).includes('有效期 30 天'));
    }
    await cancel();
    groups=[blocked,legacy,enabled];await page.reload();await open();
    assert.equal(await select().inputValue(),'');assert(await generate().isDisabled());
    await generate().evaluate(el=>el.click());assert.equal(posts.length,0);assert.equal(await page.getByRole('alertdialog').count(),0);
    await select().selectOption(enabled.id);
    await page.getByLabel('积分套餐',{exact:true}).selectOption('tier-1000');assert.equal(await select().inputValue(),enabled.id);
    await generate().click();
    const confirmation=page.getByRole('alertdialog');await confirmation.waitFor();
    for(const text of ['套餐：PRO','1,000 积分','有效期 30 天',enabled.name])assert((await confirmation.innerText()).includes(text),`confirmation mentions ${text}`);
    await confirmation.locator('[data-confirm="accept"]').click();
    await page.getByRole('dialog',{name:'新生成的卡密'}).getByText('TEST-ONLY',{exact:true}).first().waitFor();
    assert.equal(posts.length,1);assert.equal(posts[0].groupId,enabled.id);assert.equal(posts[0].templateId,'tier-1000');
    // Close the one-time results as an operator would: until then, leaving the page asks first.
    await page.getByRole('dialog',{name:'新生成的卡密'}).getByRole('button',{name:'完成',exact:true}).click();
    await page.getByRole('alertdialog').locator('[data-confirm="accept"]').click();
    await page.getByRole('dialog',{name:'新生成的卡密'}).waitFor({state:'detached'});
    await page.locator('.btn-refresh:not([disabled])').waitFor();
    for(const unavailable of [[blocked],[]]){
      groups=unavailable;await page.reload();await open();
      assert.equal(await select().inputValue(),'');assert(await select().isDisabled());assert(await generate().isDisabled());
      await generate().evaluate(el=>el.click());assert.equal(posts.length,1);
      await cancel();
    }
    groups=[legacy,blocked];await page.reload();await button('分组与权益').click();
    const checkbox=page.getByLabel('可发新卡',{exact:true});await checkbox.waitFor();assert(await checkbox.isChecked());
    await page.getByRole('button',{name:'编辑',exact:true}).nth(1).click();assert(!(await checkbox.isChecked()));
    await page.getByRole('button',{name:'编辑',exact:true}).first().click();await checkbox.uncheck();
    await page.route('**/api/v1/admin/commercial-config',async route=>{
      if(route.request().method()!=='POST')return route.fallback();
      saved=route.request().postDataJSON();groups=saved.groups;
      await route.fulfill({json:{success:true,config:{revision:'saved',groups,models:[],rate_cards:[],versions:[],audit:[]}}});
    });
    await page.getByLabel('变更原因',{exact:true}).fill('测试禁止新发卡');
    await button('发布').click();
    const publishBox=page.getByRole('alertdialog');await publishBox.waitFor();
    assert((await publishBox.innerText()).includes('测试禁止新发卡'),'the confirmation repeats the reason');
    await publishBox.locator('[data-confirm="accept"]').click();
    await page.locator('.toast').filter({hasText:'已发布'}).waitFor();
    assert.equal(saved.groups[0].issuance_enabled,false);assert.equal(saved.groups[1].issuance_enabled,false);
    assert.equal(saved.groups[0].rate_card_id,legacy.rate_card_id);
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log('PASS issuance: flag filtering, legacy/single auto-selection, explicit multiple selection, independent tiers, zero groups, summary, editor persistence');
  } finally {await browser.close();await new Promise(resolve=>server.close(resolve));}
})().catch(error=>{console.error(error);process.exitCode=1;});
