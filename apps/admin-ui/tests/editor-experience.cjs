// Editor experience regression: final build + loopback fixture, never production.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
const http=require('node:http'),fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict');
const fixture=require('./fixture-api.cjs')();
const root=path.resolve(__dirname,'../dist');
const server=http.createServer(async(req,res)=>{
  try{
    if(req.url.startsWith('/api/'))return await fixture.handle(req,res);
    const file=path.resolve(root,decodeURIComponent(req.url.split('?')[0]).replace(/^\/admin\/?/,'')||'index.html');
    if(!file.startsWith(root+path.sep)||!fs.existsSync(file)){res.writeHead(404);return res.end();}
    res.setHeader('Content-Type',file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':'text/html');res.end(fs.readFileSync(file));
  }catch(error){res.writeHead(500);res.end(JSON.stringify({error:error.message}));}
});
const until=async ready=>{const end=Date.now()+10000;while(!ready()){assert(Date.now()<end,'timed out waiting for fixture request');await new Promise(resolve=>setTimeout(resolve,10));}};
(async()=>{
  let browser;
  try{
    await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
    browser=await chromium.launch({headless:true,...(process.env.CHROME_PATH?{executablePath:process.env.CHROME_PATH}:{})});
    const page=await browser.newPage({viewport:{width:1280,height:900}}),errors=[];
    page.setDefaultTimeout(10000);
    const origin=`http://127.0.0.1:${server.address().port}`;
    page.on('pageerror',error=>{errors.push(error.message);console.error('Browser error:',error.message);});
    const nativeDialogs=[];page.on('dialog',dialog=>{nativeDialogs.push(dialog.message());void dialog.dismiss();});
    await page.route('**/*',route=>new URL(route.request().url()).origin===origin?route.continue():route.abort());
    const button=name=>page.getByRole('button',{name,exact:true});
    const nav=name=>page.getByRole('navigation').getByRole('button',{name,exact:true}).click();
    const status=text=>page.getByRole('status').filter({hasText:text}).waitFor();
    // Confirmations are the console's own dialog (role alertdialog), never window.confirm.
    const answer=async accept=>{
      const box=page.getByRole('alertdialog');await box.waitFor();
      await (accept?box.locator('[data-confirm="accept"]'):box.getByRole('button',{name:'取消',exact:true})).click();
      await box.waitFor({state:'detached'});
    };
    const toast=text=>page.locator('.toast').filter({hasText:text}).waitFor();
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();
    await button('刷新').waitFor();
    let config=(await (await page.request.get(origin+'/api/v1/admin/commercial-config')).json()).config;
    config.versions=config.versions.map(version=>({...version,margin_multiplier:1,currency:'USD',input_price_per_m:1,output_price_per_m:1,cache_read_price_per_m:1,cache_creation_price_per_m:1}));
    let failRead=false,postMode='hold',held=null,posts=[];
    await page.route('**/api/v1/admin/commercial-config',async route=>{
      if(route.request().method()==='GET')return route.fulfill(failRead?{status:503,json:{error:'fixture read unavailable'}}:{json:{success:true,config}});
      const body=route.request().postDataJSON();posts.push(body);
      // Simulate a committed write, even when its acknowledgement will be lost.
      config={...config,...(body.settings?{settings:{...config.settings,...body.settings}}:{}),...(body.models?{models:body.models}:{}),...(body.groups?{groups:body.groups}:{}),versions:[...config.versions,...(body.versions||[])],revision:`fixture-editor-${posts.length}`};
      if(postMode==='hold'){held=route;return;}
      return route.fulfill({json:{success:true,config}});
    });
    // Discovery must merge, deduplicate, preserve manual edits, and never save implicitly.
    let keyPosts=[],importPosts=0,savedKey=null,failKeysRead=false,keyWriteMode='lost';
    await page.route('**/api/v1/admin/providers',async route=>{
      if(failKeysRead)return route.fulfill({status:503,json:{error:'fixture key read unavailable'}});
      const response=await route.fetch(),body=await response.json();
      if(savedKey)body.keys[0]={...body.keys[0],...savedKey,id:savedKey.key_id};
      return route.fulfill({json:body});
    });
    await page.route('**/api/v1/admin/providers/keys',async route=>{savedKey=route.request().postDataJSON();keyPosts.push(savedKey);if(keyWriteMode==='negative')return route.fulfill({json:{success:false}});await route.abort('failed');});
    await page.route('**/api/v1/admin/providers/import',route=>{importPosts++;return route.fulfill({json:{success:true}});});
    let candidates=['candidate-model','manual-model','candidate-model'];
    await page.route('**/api/v1/admin/providers/keys/discover',route=>route.fulfill({json:{success:true,models:candidates,has_more:true}}));
    await nav('供应商与 Key');await button('编辑').first().click();
    const models=page.getByLabel('可用模型（每行一个）');
    await models.fill('manual-model\n manual-model \nprivate-model');
    await button('获取模型列表').click();await button('全部加入').click();
    assert.deepEqual((await models.inputValue()).split('\n'),['manual-model','private-model','candidate-model']);
    assert.equal(keyPosts.length,0);
    candidates=[];await button('获取模型列表').click();await status('上游没有返回模型');
    assert(await button('全部加入').isDisabled());assert((await models.inputValue()).includes('private-model'));
    const weight=page.getByLabel('权重',{exact:true});
    for(const value of ['','0','1.5','1001']){await weight.fill(value);await button('保存').click();await status('权重须为 1 至 1000 的整数');assert.equal(keyPosts.length,0);assert.equal(await page.getByRole('alertdialog').count(),0);}
    await weight.fill('3');
    // An existing Key keeps its provider and Key ID (changing them would make another Key),
    // and the provider form is not part of it.
    assert(await page.getByLabel('供应商 ID',{exact:true}).evaluate(el=>el.readOnly));assert(await page.getByLabel('Key ID',{exact:true}).evaluate(el=>el.readOnly));
    assert.equal(await page.getByLabel('上游地址',{exact:true}).count(),0);assert.equal(await button('保存供应商').count(),0);
    await page.getByLabel('API Key',{exact:true}).fill('fixture-not-a-real-secret');
    // A lost save acknowledgement locks mutations; failed review cannot clear that lock.
    await button('保存').click();await answer(true);await status('没收到保存结果');
    assert.equal(keyPosts.length,1);assert(await button('保存').isDisabled());
    failKeysRead=true;await button('重新读取').click();await answer(true);await status('核对失败');
    assert(await button('保存').isDisabled());assert((await models.inputValue()).includes('private-model'));
    failKeysRead=false;await button('重新读取').click();await answer(true);await status('已重新读取 Key 权限');
    assert.equal(await page.getByLabel('API Key',{exact:true}).inputValue(),'');
    assert.equal(await weight.inputValue(),'3');assert.equal(keyPosts.length,1);
    keyWriteMode='negative';await button('保存').click();await answer(true);await status('服务器未确认操作成功');assert(await button('保存').isEnabled());
    console.log('PASS: discovery merges without data loss; Key validation, import impact, uncertain-write lock and failed review');
    await nav('运营概览');await nav('供应商与 Key');
    await page.waitForFunction(()=>document.querySelector('#key-editor textarea')?.value.includes('private-model'));
    assert.equal(await weight.inputValue(),'3');
    // Adding a provider: only the provider form, saved by 保存供应商; the Key API is never called.
    await button('＋ 添加供应商').click();
    const editor=page.locator('#key-editor');await editor.getByRole('heading',{name:'添加供应商'}).waitFor();
    assert.equal(await editor.getByLabel('Key ID',{exact:true}).count(),0);assert.equal(await editor.getByLabel('权重',{exact:true}).count(),0);
    assert.equal(await editor.getByRole('button',{name:'保存',exact:true}).count(),0);
    const keyPostsBefore=keyPosts.length;
    await editor.getByLabel('供应商 ID',{exact:true}).fill('fixture-provider');
    await editor.getByLabel('API Key',{exact:true}).fill('fixture-not-a-real-secret');
    await editor.getByLabel('上游地址',{exact:true}).fill('http://upstream.invalid');
    await button('保存供应商').click();await status('上游地址须使用 HTTPS');assert.equal(importPosts,0);
    await editor.getByLabel('上游地址',{exact:true}).fill('https://upstream.invalid');
    // Same name as an existing provider: the confirmation says what is overwritten and reset.
    await button('保存供应商').click();
    const importBox=page.getByRole('alertdialog');await importBox.waitFor();const importConfirmation=await importBox.innerText();await answer(false);
    assert(importConfirmation.includes('fixture-provider-key-1'));assert(importConfirmation.includes('重置为启用、权重 1'));assert.equal(importPosts,0);
    // A provider that does not exist yet: discovery waits until it is saved; saving creates it.
    await editor.getByLabel('供应商 ID',{exact:true}).fill('fixture-new');
    assert(await button('获取模型列表').isDisabled());assert.equal(await button('获取模型列表').getAttribute('title'),'先保存供应商，再获取模型列表');
    await button('保存供应商').click();
    const newBox=page.getByRole('alertdialog');await newBox.waitFor();assert(!(await newBox.innerText()).includes('将覆盖'));await answer(true);
    await until(()=>importPosts===1);await editor.waitFor({state:'detached'});
    assert.equal(keyPosts.length,keyPostsBefore,'the new-provider form never calls the Key API');
    // Price template changes and malformed advanced JSON cannot erase price inputs.
    await nav('模型与定价');
    const tokenContext=page.getByLabel('上下文长度',{exact:true}),tokenOutput=page.getByLabel('最大输出',{exact:true});
    await tokenContext.waitFor();
    const originalContext=await tokenContext.inputValue(),originalOutput=await tokenOutput.inputValue();
    await tokenContext.fill('1M');await tokenOutput.fill('128K');
    assert.equal(await tokenContext.inputValue(),'1000000');assert.equal(await tokenOutput.inputValue(),'128000');
    const tokenHint=page.locator('.mapping-editor .field-hint').filter({hasText:'1M Tokens'});
    assert(await tokenHint.isVisible());assert.equal(await tokenHint.getAttribute('title'),'1M Tokens（1,000,000）');
    await tokenContext.fill(originalContext);await tokenOutput.fill(originalOutput);
    await page.getByRole('button',{name:'编辑',exact:true}).first().click();
    assert(await page.locator('.mapping-editor').evaluate(element=>element===document.activeElement));
    // Advanced JSON stays folded until asked for.
    assert.equal(await page.locator('details.json-details').first().getAttribute('open'),null);
    const template=page.getByLabel('选择价格版本模板'),version=page.getByLabel('新版本 ID',{exact:true}),date=page.getByLabel('生效时间（本地时区）',{exact:true});
    await template.selectOption('fixture-price-0');await version.fill('editor-price');await date.fill('2099-01-01T10:00');
    await template.selectOption('fixture-price-1');await answer(false);assert.equal(await template.inputValue(),'fixture-price-0');assert.equal(await version.inputValue(),'editor-price');assert.equal(await date.inputValue(),'2099-01-01T10:00');
    await page.getByText('编辑 JSON（高级）',{exact:true}).click();
    const json=page.getByLabel('配置 JSON',{exact:true}),original=await json.inputValue();
    await json.fill('{ broken');await button('加入价格草稿').click();await status('高级配置 JSON 无效');assert.equal(await json.inputValue(),'{ broken');assert.equal(await version.inputValue(),'editor-price');
    await json.fill(original);const multiplier=page.getByLabel('版本倍率',{exact:true});
    await multiplier.fill('0');await button('加入价格草稿').click();await status('需大于 0、不超过 1000');
    await multiplier.fill('1');await date.fill('2000-01-01T10:00');await button('加入价格草稿').click();await status('生效时间需晚于现在');
    await date.fill('2099-01-01T10:00');await button('加入价格草稿').click();await status('已加入 1 个价格版本');
    const staged=JSON.parse(await json.inputValue()).versions[0];
    await template.selectOption('fixture-price-1');await version.fill('editor-price');await date.fill('2099-01-02T10:00');await button('加入价格草稿').click();await status('版本 ID 已在本页草稿中');assert.deepEqual(JSON.parse(await json.inputValue()).versions,[staged]);
    await button('取消价格编辑').click();await answer(true);
    const reason=page.getByLabel('变更原因',{exact:true});
    // '变' is 3 bytes: 167 of them exceed the 500-byte limit.
    await reason.fill('变'.repeat(167));await button('发布').click();await status('最多约 160 字');assert.equal(posts.length,0);
    await reason.fill('fixture editor validation');const context=page.getByLabel('上下文长度',{exact:true});
    await context.fill('');await button('发布').click();await status('上下文长度须为');assert.equal(await context.inputValue(),'');assert.equal(posts.length,0);
    assert.equal(await page.getByRole('alertdialog').count(),0,'invalid drafts never reach the confirmation');
    assert.equal(JSON.parse(await json.inputValue()).models[0].context_window,'');
    await context.fill('200000');
    console.log('PASS: template cancel, invalid JSON, nonpositive rates, retroactive dates, staged ID collision and blank numeric inputs preserve drafts');
    // Reentrant clicks send only one publication, and an unsuccessful reread keeps the draft.
    await button('发布').evaluate(element=>{element.click();element.click();});
    await answer(true);await until(()=>held);
    assert.equal(await page.getByRole('alertdialog').count(),0,'a second click never opens a second confirmation');
    await button('发布中…').evaluate(element=>{element.click();element.click();});
    assert.equal(posts.length,1);assert(await button('发布中…').isDisabled());
    await held.abort('failed');held=null;await status('没收到发布结果');assert(await button('发布').isDisabled());
    const beforeReload=await json.inputValue();failRead=true;await button('放弃修改').click();await answer(true);await status('修改已保留');
    assert.equal(await json.inputValue(),beforeReload);assert(await button('发布').isDisabled());assert.equal(posts.length,1);
    failRead=false;await button('放弃修改').click();await answer(true);await toast('已重新加载配置');assert.deepEqual(JSON.parse(await json.inputValue()).versions,[]);
    // With nothing unpublished, 放弃修改 is not offered, and the console's 刷新 reloads this page too.
    assert.equal(await button('放弃修改').count(),0);
    config={...config,models:config.models.map((row,index)=>index===0?{...row,display_name:'刷新后的显示名'}:row),revision:'fixture-editor-refreshed'};
    await button('刷新').click();await page.locator('.mapping-table, table').getByText('刷新后的显示名').first().waitFor();
    // …but never over unpublished edits: it says the server may have changed and offers to discard.
    const contextInput=page.getByLabel('上下文长度',{exact:true});const keptContext=await contextInput.inputValue();await contextInput.fill('150000');
    config={...config,models:config.models.map((row,index)=>index===0?{...row,display_name:'又一次更新'}:row),revision:'fixture-editor-refreshed-2'};
    await button('刷新').click();await page.getByText('服务器上的配置可能已更新').waitFor();
    assert.equal(await contextInput.inputValue(),'150000');assert.equal(await page.getByText('又一次更新').count(),0);
    await button('放弃修改并加载').click();await answer(true);await page.getByText('又一次更新').first().waitFor();
    assert.equal(await contextInput.inputValue(),keptContext);assert.equal(posts.length,1);
    console.log('PASS: one in-flight publication; lost acknowledgement locks publish until a successful, confirmed reread');
    // Financials reject invalid values and retain drafts when review itself fails.
    await nav('财务对账');const face=page.getByLabel('积分面值',{exact:true}),financialReason=page.getByLabel('变更原因',{exact:true});
    await face.fill('0');await financialReason.fill('fixture invalid');assert(await button('发布').isDisabled());
    await face.fill('0.02');await financialReason.fill('变'.repeat(167));assert(await button('发布').isDisabled());assert.equal(posts.length,1);
    await financialReason.fill('fixture finance write');await page.getByLabel('美元汇率',{exact:true}).fill('7.3');
    await button('发布').click();
    const financeBox=page.getByRole('alertdialog');await financeBox.waitFor();const financeConfirmation=await financeBox.innerText();
    await answer(true);await until(()=>held);assert(financeConfirmation.includes('0.01 → 0.02'));assert(financeConfirmation.includes('7.2 → 7.3'));
    await held.abort('failed');held=null;await status('没收到发布结果');assert.equal(posts.length,2);assert(await button('发布').isDisabled());
    failRead=true;await button('放弃修改').click();await answer(true);await status('修改已保留；重新加载成功前不能发布');
    assert.equal(await face.inputValue(),'0.02');assert.equal(await financialReason.inputValue(),'fixture finance write');assert(await button('发布').isDisabled());
    failRead=false;await button('放弃修改').click();await answer(true);await toast('已重新加载结算参数');assert.equal(await face.inputValue(),'0.02');
    // Success followed by a read failure must never be reported as an uncertain publication.
    postMode='success';await page.route('**/api/v1/admin/financials',route=>route.fulfill({status:503,json:{error:'fixture estimates unavailable'}}));
    await face.fill('0.03');await financialReason.fill('fixture successful publish');await button('发布').click();await answer(true);await toast('已发布结算参数');
    await page.getByRole('alert').filter({hasText:'部分数据加载失败'}).waitFor();
    assert.equal(await page.getByText('没收到发布结果').count(),0,'a failed estimates refresh is not an uncertain publication');
    assert.equal(posts.length,3);assert.equal(await face.inputValue(),'0.03');assert.equal(await financialReason.inputValue(),'');
    await financialReason.fill('unchanged');await button('发布').click();await status('数值与当前版本一致');assert.equal(posts.length,3);
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log('PASS: financial validation, before/after confirmation, failed-read draft retention and publish success distinct from estimates refresh');
  }finally{if(browser)await browser.close();await new Promise(resolve=>server.close(resolve));}
})().catch(error=>{console.error(error);process.exitCode=1;});
