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
    const page=await browser.newPage({viewport:{width:390,height:844}});
    const origin=`http://127.0.0.1:${server.address().port}`, errors=[];
    page.on('pageerror',e=>errors.push(e.message));
    const nativeDialogs=[];page.on('dialog',d=>{nativeDialogs.push(d.message());void d.dismiss();});
    await page.route('**/*',route=>new URL(route.request().url()).origin===origin?route.continue():route.abort());
    const nav=name=>page.getByRole('button',{name,exact:true}).click();
    // Confirmations are the console's own dialog (role alertdialog), never window.confirm.
    const answer=async accept=>{
      const box=page.getByRole('alertdialog');await box.waitFor();
      await (accept?box.locator('[data-confirm="accept"]'):box.getByRole('button',{name:'取消',exact:true})).click();
      await box.waitFor({state:'detached'});
    };
    const batchDialog=page.getByRole('dialog',{name:'批量生成卡密'}),generate=batchDialog.locator('.modal-actions .btn-primary');
    const login=async()=>{await page.getByLabel('密码',{exact:true}).fill('fixture-password');await nav('登录');await nav('卡密资产');await page.getByRole('button',{name:'查看卡密',exact:true}).first().waitFor();};
    await page.goto(origin+'/admin/');await login();
    // A committed batch with a lost response must remain locked even after reload.
    let posts=0;
    await page.route('**/api/v1/admin/cards/batch',async route=>{posts++;await route.fetch();await route.abort('failed');});
    await nav('＋ 批量生成');await page.getByLabel('模型与计费分组',{exact:true}).selectOption('fixture-group-0');await page.getByLabel('生成数量',{exact:true}).fill('3');await nav('生成 3 张');await answer(true);
    await page.getByRole('region',{name:'制卡结果核对'}).waitFor();assert.equal(posts,1);
    await page.reload();await nav('卡密资产');
    await page.getByRole('region',{name:'制卡结果核对'}).waitFor();
    // While that result is unconfirmed, 批量生成 says why instead of opening a form that cannot be sent.
    const openBatch=page.getByRole('button',{name:'＋ 批量生成',exact:true});assert(await openBatch.isDisabled());
    assert.equal(await openBatch.getAttribute('title'),'上次批量生成的结果未确认，请先在列表上方核对');
    await nav('查看这批卡');
    assert((await page.getByRole('textbox',{name:'搜索卡密',exact:true}).inputValue()).startsWith('批次 '));
    assert.equal(await page.locator('tbody tr').count(),3);
    const unlock=page.getByRole('button',{name:'已核对，继续制卡',exact:true});assert(await unlock.isDisabled());
    await page.route('**/api/v1/admin/cards?*',route=>route.fulfill({status:503,json:{error:'fixture unavailable'}}));
    await nav('刷新列表');await page.getByRole('alert').filter({hasText:'卡密核对刷新失败'}).waitFor();assert(await unlock.isDisabled());
    await page.unroute('**/api/v1/admin/cards?*');await nav('刷新列表');await page.waitForFunction(()=>[...document.querySelectorAll('button')].some(b=>b.textContent==='已核对，继续制卡'&&!b.disabled));
    await unlock.click();await answer(false);assert.equal(await page.getByRole('region',{name:'制卡结果核对'}).count(),1,'cancelling keeps the lock');
    await unlock.click();await answer(true);assert.equal(posts,1);
    assert.equal(await page.evaluate(()=>sessionStorage.getItem('admin-pending-issuance:v1')),null);
    // Save, navigate away and return: only latest server data may initialize the editor.
    let savedKey=null;
    await page.route('**/api/v1/admin/providers',async route=>{const response=await route.fetch();const body=await response.json();if(savedKey)body.keys[0]={...body.keys[0],...savedKey,id:savedKey.key_id};await route.fulfill({json:body});});
    await page.route('**/api/v1/admin/providers/keys',async route=>{savedKey=route.request().postDataJSON();await route.fulfill({json:{success:true}});});
    await nav('供应商与 Key');await page.getByRole('button',{name:'编辑',exact:true}).first().click();
    const models=page.getByLabel('可用模型（每行一个）');await nav('手动输入');await models.fill('new-model');
    // Waits until exactly these models are ticked in the Key's checklist.
    const ticked=async expected=>{await page.waitForFunction(names=>{const boxes=[...document.querySelectorAll('#key-editor .model-checklist input[type=checkbox]')];return JSON.stringify(boxes.filter(box=>box.checked).map(box=>box.getAttribute('aria-label')))===JSON.stringify(names);},expected);return expected;};await page.getByLabel('启用',{exact:true}).uncheck();
    await nav('保存');await answer(true);await page.getByRole('status').filter({hasText:'已保存 Key'}).waitFor();
    await page.waitForFunction(()=>document.querySelector('table')?.textContent.includes('new-model'));
    await nav('运营概览');await nav('供应商与 Key');await page.getByRole('group',{name:'可用模型'}).waitFor();
    await ticked(['new-model']);assert.equal(await page.getByLabel('启用',{exact:true}).isChecked(),false);
    await page.route('**/api/v1/admin/providers',route=>route.fulfill({status:503,json:{error:'refresh unavailable'}}));
    await nav('手动输入');await models.fill('saved-despite-refresh-failure');await nav('保存');await answer(true);
    await page.getByRole('alert').filter({hasText:'部分数据加载失败'}).waitFor();
    await nav('运营概览');await nav('供应商与 Key');
    await ticked(['saved-despite-refresh-failure']);
    assert.equal(await page.getByLabel('启用',{exact:true}).isChecked(),false);
    // Re-opening the Key being edited keeps the draft and its leave guard.
    await nav('手动输入');await models.fill('unsaved-model');await page.getByRole('button',{name:'编辑',exact:true}).first().click();
    assert.equal(await models.inputValue(),'unsaved-model');
    await nav('运营概览');await answer(false);assert.equal(await models.inputValue(),'unsaved-model');
    await nav('调用追踪');await answer(true);
    await page.getByRole('button',{name:'详情',exact:true}).first().click();
    await page.waitForFunction(()=>document.activeElement?.id==='trace-detail');
    assert.equal(await page.evaluate(()=>document.activeElement?.id),'trace-detail');
    // On a phone the detail covers the screen; Escape (or 关闭详情) returns to the list and navigation.
    await page.keyboard.press('Escape');await page.locator('#trace-detail').waitFor({state:'detached'});
    await nav('财务对账');
    const face=page.getByLabel('积分面值',{exact:true});await face.fill('0.8');
    await nav('运营概览');await answer(false);assert.equal(await face.inputValue(),'0.8');
    await nav('放弃修改');await answer(true);
    await page.waitForFunction(()=>!document.querySelector('fieldset')?.disabled);
    await nav('运营概览');await page.getByRole('heading',{name:'运营概览',exact:true}).waitFor();
    // A known pre-commit insufficient-balance rejection must not lock unrelated cards.
    await nav('安全与审计');await nav('退出登录');await login();
    let adjustments=0;
    // The balance shown is 2000, but it dropped to 500 in the meantime: the server refuses before any commit.
    await page.route('**/api/v1/admin/cards/adjust',route=>{adjustments++;return route.fulfill({status:409,json:{success:false,error:'Card error: Insufficient credit: available 500000000 micro-credits, needed 1000000000'}});});
    await page.getByRole('button',{name:'调账',exact:true}).first().click();
    await page.getByRole('radio',{name:'扣减',exact:true}).click();
    // Deducting past the balance shown is stopped before review: the server would only refuse it.
    await page.getByLabel('增减积分数量').fill('3000');await page.getByLabel('调账原因说明').fill('test rejection');
    const next=page.getByRole('button',{name:'下一步',exact:true});assert(await next.isDisabled());assert.equal(await next.getAttribute('title'),'余额不足');
    assert.equal(adjustments,0);
    await page.getByLabel('增减积分数量').fill('1000');
    await nav('下一步');await nav('确认入账');
    await page.getByRole('dialog').getByRole('alert').filter({hasText:'服务器拒绝了这笔调账'}).waitFor();
    assert.equal(await page.getByLabel('增减积分数量').isDisabled(),false);assert.equal(adjustments,1);
    assert.equal(await page.evaluate(()=>Object.keys(sessionStorage).filter(k=>k.includes('pending-adjustment')).length),0);
    await nav('取消');await page.getByRole('button',{name:'调账',exact:true}).nth(1).click();
    assert.equal(await page.getByLabel('增减积分数量').isDisabled(),false);await nav('取消');
    // A price changes in its own drawer, published on its own; unpublished page edits come first.
    await nav('模型与定价');
    const priced=page.getByRole('row').filter({hasText:'claude-sonnet'}).filter({has:page.getByRole('button',{name:'调价'})});
    await page.getByLabel('上下文长度',{exact:true}).fill('150000');
    const priceButton=priced.getByRole('button',{name:'调价',exact:true});
    assert(await priceButton.isDisabled());assert.equal(await priceButton.getAttribute('title'),'先发布或放弃未发布的修改，再调价');
    await nav('放弃修改');await answer(true);await page.waitForFunction(()=>![...document.querySelectorAll('button')].some(b=>b.textContent==='放弃修改'));
    const config=(await (await page.request.get(origin+'/api/v1/admin/commercial-config')).json()).config;let publishBodies=[];
    await page.route('**/api/v1/admin/commercial-config',route=>{if(route.request().method()!=='POST')return route.continue();const body=route.request().postDataJSON();publishBodies.push(body);return route.fulfill({json:{success:true,config:{...config,revision:'fixture-after-price',versions:[...config.versions,...body.versions]}}});});
    await priceButton.click();
    const drawer=page.locator('#price-drawer');await drawer.waitFor();
    await drawer.getByLabel('新输入售价',{exact:true}).fill('2.5');await drawer.getByLabel('调价原因',{exact:true}).fill('test new price');
    // The legacy fixture has no procurement prices: they are asked for, and nothing is sent until given.
    await drawer.getByRole('button',{name:'发布调价',exact:true}).click();
    await drawer.getByRole('alert').filter({hasText:'采购价需在'}).waitFor();assert.equal(await page.getByRole('alertdialog').count(),0);assert.equal(publishBodies.length,0);
    for(const label of ['输入','输出','缓存写','缓存读'])await drawer.getByLabel(`采购${label}价`,{exact:true}).fill('1');
    await drawer.getByRole('button',{name:'发布调价',exact:true}).click();await answer(true);
    await page.locator('.toast').filter({hasText:'已发布 claude-sonnet 的新价格'}).waitFor();
    assert.equal(publishBodies.length,1);assert.equal(publishBodies[0].expected_revision,config.revision);assert.equal(publishBodies[0].reason,'test new price');
    const version=publishBodies[0].versions[0];
    assert.match(version.id,/^claude-sonnet-\d{12}$/);assert.equal(version.margin_multiplier,1);assert.equal(version.fixed_input_credit_per_m,2500000);assert.equal(version.currency,'USD');
    console.log('PASS: definite insufficient-balance rejection clears only uncommitted intent; another card remains editable; price changes wait for page edits; missing procurement prices block the send; the new version carries the generated ID');
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log('PASS: committed issuance lost response locks across reload; failed review cannot unlock; successful review never submits; saved Key remount uses latest permissions; same-Key edit preserves dirty guard; trace details receive focus');
  } finally {await browser.close();}
})().catch(e=>{console.error(e);process.exitCode=1;}).finally(()=>server.close());
