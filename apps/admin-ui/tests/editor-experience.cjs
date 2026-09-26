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
    config.versions.push({...config.versions[0],id:'fixture-price-0-old',effective_from_secs:config.versions[0].effective_from_secs-86400});
    let failRead=false,postMode='hold',held=null,posts=[],nextPublishReply=null;
    await page.route('**/api/v1/admin/commercial-config',async route=>{
      if(route.request().method()==='GET')return route.fulfill(failRead?{status:503,json:{error:'fixture read unavailable'}}:{json:{success:true,config}});
      const body=route.request().postDataJSON();posts.push(body);
      if(nextPublishReply){const reply=nextPublishReply;nextPublishReply=null;return route.fulfill(reply);}
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
    // Typing the list stays available behind 手动输入.
    await button('手动输入').click();
    const models=page.getByLabel('可用模型（每行一个）');
    await models.fill('manual-model\n manual-model \nprivate-model');
    await button('获取模型列表').click();await button('全部加入').click();
    assert.deepEqual((await models.inputValue()).split('\n'),['manual-model','private-model','candidate-model']);
    assert.equal(keyPosts.length,0);
    // The same list as a checklist: ticked means authorised; what 获取模型列表 found is marked 新.
    const picker=page.getByRole('group',{name:'可用模型'});
    assert(await picker.getByRole('checkbox',{name:'candidate-model',exact:true}).isChecked());
    await picker.locator('li').filter({hasText:'candidate-model'}).getByText('新',{exact:true}).waitFor();
    await picker.getByRole('checkbox',{name:'private-model',exact:true}).uncheck();
    assert.deepEqual((await models.inputValue()).split('\n'),['manual-model','candidate-model']);
    await picker.getByRole('button',{name:'全不选',exact:true}).click();assert.equal(await models.inputValue(),'');
    await page.getByText('未选择模型：这个 Key 不会被使用').waitFor();
    assert.equal(await picker.getByRole('checkbox',{name:'private-model',exact:true}).count(),1,'an unticked typed-in model stays listed');
    await picker.getByRole('button',{name:'全选',exact:true}).click();
    assert((await models.inputValue()).split('\n').includes('private-model'));
    await models.fill('manual-model\nprivate-model\ncandidate-model');
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
    // After remounting, the editor shows the saved permissions (ticked), read from the latest data.
    await page.getByRole('group',{name:'可用模型'}).getByRole('checkbox',{name:'private-model',exact:true,checked:true}).waitFor();
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
    // Models: explicit token units; routing picked from lists; an edited input shows what it was.
    await nav('模型与定价');
    const tokenContext=page.getByLabel('上下文长度',{exact:true}),tokenOutput=page.getByLabel('最大输出',{exact:true});
    await tokenContext.waitFor();
    const originalContext=await tokenContext.inputValue(),originalOutput=await tokenOutput.inputValue();
    await tokenContext.fill('1M');await tokenOutput.fill('128K');
    assert.equal(await tokenContext.inputValue(),'1000000');assert.equal(await tokenOutput.inputValue(),'128000');
    const tokenHint=page.locator('.mapping-editor .field-hint').filter({hasText:'1M Tokens'});
    assert(await tokenHint.isVisible());assert.equal(await tokenHint.getAttribute('title'),'1M Tokens（1,000,000）');
    await page.getByText('原 200K',{exact:true}).waitFor();
    await tokenContext.fill(originalContext);await tokenOutput.fill(originalOutput);
    // Nothing changed any more: no publish bar.
    assert.equal(await page.getByRole('region',{name:'发布'}).count(),0);
    await page.getByRole('button',{name:'编辑',exact:true}).first().click();
    assert(await page.locator('.mapping-editor').evaluate(element=>element===document.activeElement));
    const providerSelect=page.locator('.mapping-editor').getByLabel('供应商',{exact:true});
    assert.deepEqual(await providerSelect.locator('option').evaluateAll(options=>options.map(option=>option.value)),['fixture-provider','fixture-disabled','fixture-openai']);
    await providerSelect.selectOption('fixture-openai');
    const upstream=page.locator('.mapping-editor').getByLabel('上游模型',{exact:true});
    assert.deepEqual(await page.locator(`datalist#${await upstream.getAttribute('list')} option`).evaluateAll(options=>options.map(option=>option.value)),['gpt-5.6-sol','gpt-5.6-terra','gpt-6-astra']);
    await page.getByText('这个供应商的 Key 还没有授权此模型').waitFor();
    await upstream.fill('my-custom-upstream');assert.equal(await upstream.inputValue(),'my-custom-upstream');
    await page.getByText('原 claude-sonnet',{exact:true}).waitFor();await page.getByText('原 测试供应商 / Fixture',{exact:true}).waitFor();
    await providerSelect.selectOption('fixture-provider');await upstream.fill('claude-sonnet');
    assert.equal(await page.getByRole('region',{name:'发布'}).count(),0,'back to the original: nothing to publish');
    // Advanced JSON lives in a drawer from the top bar; malformed JSON blocks publishing and stays as typed.
    await button('JSON').click();
    const json=page.getByLabel('配置 JSON',{exact:true}),original=await json.inputValue();
    await json.fill('{ broken');await page.getByRole('alert').filter({hasText:'配置 JSON 格式无效'}).waitFor();
    // The drawer is where JSON is edited; publishing stays with the page's bar.
    await page.keyboard.press('Escape');await page.locator('#config-json').waitFor({state:'detached'});
    const reason=page.getByLabel('变更原因',{exact:true});
    await reason.fill('fixture editor validation');await button('发布').click();await status('配置 JSON 格式无效');assert.equal(posts.length,0);
    await button('JSON').click();assert.equal(await json.inputValue(),'{ broken');
    await json.fill(original);await page.keyboard.press('Escape');
    const context=page.getByLabel('上下文长度',{exact:true});
    await context.fill('');
    // '变' is 3 bytes: 167 of them exceed the 500-byte limit.
    await reason.fill('变'.repeat(167));await button('发布').click();await status('最多约 160 字');assert.equal(posts.length,0);
    await reason.fill('fixture editor validation');await button('发布').click();await status('上下文长度须为');assert.equal(await context.inputValue(),'');assert.equal(posts.length,0);
    assert.equal(await page.getByRole('alertdialog').count(),0,'invalid drafts never reach the confirmation');
    await button('JSON').click();assert.equal(JSON.parse(await json.inputValue()).models[0].context_window,'');
    await page.keyboard.press('Escape');await page.locator('#config-json').waitFor({state:'detached'});
    await context.fill('150000');
    console.log('PASS: token units, provider and upstream lists with custom values, edited-field hints, JSON drawer, invalid JSON, reason bytes and blank numeric inputs preserve drafts');
    // Reentrant clicks send only one publication, and an unsuccessful reread keeps the draft.
    await button('发布').evaluate(element=>{element.click();element.click();});
    await answer(true);await until(()=>held);
    assert.equal(await page.getByRole('alertdialog').count(),0,'a second click never opens a second confirmation');
    await button('发布中…').evaluate(element=>{element.click();element.click();});
    assert.equal(posts.length,1);assert(await button('发布中…').isDisabled());
    await held.abort('failed');held=null;await status('没收到发布结果');assert(await button('发布').isDisabled());
    // The JSON is read from its drawer, which is closed again for the page's own buttons.
    const readJson=async()=>{await button('JSON').click();const value=await json.inputValue();await page.keyboard.press('Escape');await page.locator('#config-json').waitFor({state:'detached'});return value;};
    const beforeReload=await readJson();failRead=true;await button('放弃修改').click();await answer(true);await status('修改已保留');
    assert.equal(await readJson(),beforeReload);assert(await button('发布').isDisabled());assert.equal(posts.length,1);
    failRead=false;await button('放弃修改').click();await answer(true);await toast('已重新加载配置');assert.deepEqual(JSON.parse(await readJson()).versions,[]);
    // With nothing unpublished, 放弃修改 is not offered, and the console's 刷新 reloads this page too.
    assert.equal(await button('放弃修改').count(),0);
    config={...config,models:config.models.map((row,index)=>index===0?{...row,display_name:'刷新后的显示名'}:row),revision:'fixture-editor-refreshed'};
    // (Below 1440 the display name lives in the model cell's tooltip.)
    await button('刷新').click();await page.locator('td[title="显示名：刷新后的显示名"]').waitFor();
    // …but never over unpublished edits: it says the server may have changed and offers to discard.
    const contextInput=page.getByLabel('上下文长度',{exact:true});const keptContext=await contextInput.inputValue();await contextInput.fill('160000');
    config={...config,models:config.models.map((row,index)=>index===0?{...row,display_name:'又一次更新'}:row),revision:'fixture-editor-refreshed-2'};
    await button('刷新').click();await page.getByText('服务器上的配置可能已更新').waitFor();
    assert.equal(await contextInput.inputValue(),'160000');assert.equal(await page.getByText('又一次更新').count(),0);
    await button('放弃修改并加载').click();await answer(true);await page.locator('td[title="显示名：又一次更新"]').waitFor();
    assert.equal(await contextInput.inputValue(),keptContext);assert.equal(posts.length,1);
    // 调价: one drawer, one step, published on its own against the version read.
    postMode='success';
    const priced=page.getByRole('row').filter({hasText:'claude-sonnet'}).filter({has:page.getByRole('button',{name:'调价'})});
    await priced.getByRole('button',{name:'调价',exact:true}).click();
    const drawer=page.locator('#price-drawer');await drawer.waitFor();
    assert.equal(await drawer.getByLabel('新输入售价',{exact:true}).inputValue(),'3');assert.equal(await drawer.getByLabel('新输出售价',{exact:true}).inputValue(),'15');
    await drawer.getByLabel('新输出售价',{exact:true}).fill('12');
    await drawer.locator('.price-change').getByText('−20%',{exact:true}).waitFor();
    const sampleText=await drawer.getByRole('status').innerText();
    assert(sampleText.includes('积分（≈ ¥')&&/毛利约 -?\d+%/.test(sampleText),sampleText);
    const generatedId=(await drawer.locator('.price-advanced summary .mono').innerText()).trim();
    assert.match(generatedId,/^claude-sonnet-\d{12}$/);
    // A reason is required; cancelling the confirmation or the drawer sends nothing.
    const publishPrice=drawer.getByRole('button',{name:'发布调价',exact:true});
    assert(await publishPrice.isDisabled());
    await drawer.getByLabel('调价原因',{exact:true}).fill('输出价下调 20%');
    await publishPrice.click();
    const priceConfirm=page.getByRole('alertdialog');await priceConfirm.waitFor();
    const priceFacts=await priceConfirm.innerText();
    assert(priceFacts.includes('输出 15 → 12 积分/百万')&&priceFacts.includes('原因：输出价下调 20%')&&priceFacts.includes(generatedId.slice(0,-4)),priceFacts);
    assert(!priceFacts.includes('输入 3 →'),'unchanged prices are not listed');
    await answer(false);assert.equal(posts.length,1);
    // Changed elsewhere meanwhile: refused, nothing written, the typed prices stay; reload, then it goes through.
    nextPublishReply={status:409,json:{success:false,error:'Invalid configuration: Configuration changed; reload before publishing'}};
    await publishPrice.click();await answer(true);
    await drawer.getByRole('alert').filter({hasText:'配置刚被更新'}).waitFor();
    assert.equal(await drawer.getByLabel('新输出售价',{exact:true}).inputValue(),'12','typed prices survive a refusal');assert.equal(posts.length,2);
    await drawer.getByRole('button',{name:'重新加载',exact:true}).click();await drawer.getByRole('alert').waitFor({state:'detached'});
    const revisionRead=config.revision;
    await publishPrice.click();await answer(true);
    await drawer.waitFor({state:'detached'});await toast('已发布 claude-sonnet 的新价格');
    const sent=posts.at(-1);
    assert.deepEqual(Object.keys(sent).sort(),['expected_revision','reason','versions']);
    assert.equal(sent.expected_revision,revisionRead);assert.equal(sent.reason,'输出价下调 20%');
    assert.equal(sent.versions.length,1);const priceVersion=sent.versions[0];
    assert.match(priceVersion.id,/^claude-sonnet-\d{12}$/);assert.equal(priceVersion.model,'claude-sonnet');assert.equal(priceVersion.rate_card_id,'fixture-rate');
    assert.equal(priceVersion.fixed_output_credit_per_m,12000000);assert.equal(priceVersion.fixed_input_credit_per_m,3000000);assert.equal(priceVersion.pricing_mode,'fixed');
    assert(priceVersion.effective_from_secs>Date.now()/1000+240&&priceVersion.effective_from_secs<=Date.now()/1000+360,'five minutes after publishing');
    // The versions list: the new price as scheduled; superseded versions only with 显示历史.
    const versionsPanel=page.getByRole('region',{name:'价格版本'});
    await versionsPanel.getByRole('row').filter({hasText:'已排期'}).first().waitFor();
    assert.equal(await versionsPanel.getByRole('row').filter({hasText:'已被替代'}).count(),0,'superseded versions are hidden by default');
    await versionsPanel.getByRole('checkbox',{name:/显示历史/}).check();
    await versionsPanel.getByRole('row').filter({hasText:'已被替代'}).first().waitFor();
    postMode='hold';
    console.log('PASS: price drawer: current vs new with change and sample yuan/margin, generated ID, required reason, cancel, conflict refusal kept inputs, publish with expected_revision; versions list with 显示历史');
    console.log('PASS: one in-flight publication; lost acknowledgement locks publish until a successful, confirmed reread');
    // Financials reject invalid values and retain drafts when review itself fails.
    await nav('财务对账');const face=page.getByLabel('积分面值',{exact:true}),financialReason=page.getByLabel('变更原因',{exact:true});
    const financeBase=posts.length;
    await face.fill('0');await financialReason.fill('fixture invalid');assert(await button('发布').isDisabled());
    await face.fill('0.02');await financialReason.fill('变'.repeat(167));assert(await button('发布').isDisabled());assert.equal(posts.length,financeBase);
    await financialReason.fill('fixture finance write');await page.getByLabel('美元汇率',{exact:true}).fill('7.3');
    await button('发布').click();
    const financeBox=page.getByRole('alertdialog');await financeBox.waitFor();const financeConfirmation=await financeBox.innerText();
    await answer(true);await until(()=>held);assert(financeConfirmation.includes('0.01 → 0.02'));assert(financeConfirmation.includes('7.2 → 7.3'));
    await held.abort('failed');held=null;await status('没收到发布结果');assert.equal(posts.length,financeBase+1);assert(await button('发布').isDisabled());
    failRead=true;await button('放弃修改').click();await answer(true);await status('修改已保留；重新加载成功前不能发布');
    assert.equal(await face.inputValue(),'0.02');assert.equal(await financialReason.inputValue(),'fixture finance write');assert(await button('发布').isDisabled());
    failRead=false;await button('放弃修改').click();await answer(true);await toast('已重新加载结算参数');assert.equal(await face.inputValue(),'0.02');
    // Success followed by a read failure must never be reported as an uncertain publication.
    postMode='success';await page.route('**/api/v1/admin/financials',route=>route.fulfill({status:503,json:{error:'fixture estimates unavailable'}}));
    await face.fill('0.03');await financialReason.fill('fixture successful publish');await button('发布').click();await answer(true);await toast('已发布结算参数');
    await page.getByRole('alert').filter({hasText:'部分数据加载失败'}).waitFor();
    assert.equal(await page.getByText('没收到发布结果').count(),0,'a failed estimates refresh is not an uncertain publication');
    assert.equal(posts.length,financeBase+2);assert.equal(await face.inputValue(),'0.03');assert.equal(await financialReason.inputValue(),'');
    await financialReason.fill('unchanged');await button('发布').click();await status('数值与当前版本一致');assert.equal(posts.length,financeBase+2);
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log('PASS: financial validation, before/after confirmation, failed-read draft retention and publish success distinct from estimates refresh');
  }finally{if(browser)await browser.close();await new Promise(resolve=>server.close(resolve));}
})().catch(error=>{console.error(error);process.exitCode=1;});
