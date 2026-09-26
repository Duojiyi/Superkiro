// Route health regression: final build + loopback fixture, never production.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
const http=require('node:http'),fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict');
const fixture=require('./fixture-api.cjs')();
const root=path.resolve(__dirname,'../dist');
const diagRequests=[],diagConsole=[],t0=Date.now();
const server=http.createServer(async(req,res)=>{
  if(req.url.startsWith('/api/'))diagRequests.push([Date.now()-t0,req.method,req.url]);
  try{
    if(req.url.startsWith('/api/'))return await fixture.handle(req,res);
    const file=path.resolve(root,decodeURIComponent(req.url.split('?')[0]).replace(/^\/admin\/?/,'')||'index.html');
    if(!file.startsWith(root+path.sep)||!fs.existsSync(file)){res.writeHead(404);return res.end();}
    res.setHeader('Content-Type',file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':'text/html');res.end(fs.readFileSync(file));
  }catch(error){res.writeHead(500);res.end(JSON.stringify({error:error.message}));}
});
const until=async ready=>{const end=Date.now()+10000;while(!ready()){assert(Date.now()<end,'timed out waiting for a fixture write');await new Promise(resolve=>setTimeout(resolve,10));}};
const key=id=>fixture.keys.find(row=>row.id===id),model=id=>fixture.config.models.find(row=>row.id===id);
const writes=endpoint=>fixture.writes.filter(write=>write.endpoint===endpoint);
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
    const refresh=async()=>{await button('刷新').click();await page.locator('.btn-refresh:not([disabled])').waitFor();};
    // Opens the confirmation, returns its text, optionally ticks or unticks its option, then answers.
    const confirm=async({accept=true,option}={})=>{
      const box=page.getByRole('alertdialog');await box.waitFor();const text=await box.innerText();
      if(option!==undefined&&!(await box.getByRole('checkbox').count())){
        console.error('DIAG dialog text: '+JSON.stringify(text));
        console.error('DIAG dialogs: '+JSON.stringify(await page.evaluate(()=>[...document.querySelectorAll('[role=alertdialog],[role=dialog]')].map(e=>({role:e.getAttribute('role'),label:e.getAttribute('aria-label'),text:e.innerText.slice(0,400),underHidden:!!e.closest('[aria-hidden=true]')})))));
        console.error('DIAG shell aria-hidden: '+JSON.stringify(await page.evaluate(()=>document.querySelector('.workspace-shell')?.getAttribute('aria-hidden'))));
        console.error('DIAG editor: '+JSON.stringify(await page.evaluate(()=>({title:document.querySelector('#key-editor h3')?.textContent,checks:[...document.querySelectorAll('#key-editor .model-checklist input[type=checkbox]')].map(e=>[e.getAttribute('aria-label'),e.checked])}))));
        console.error('DIAG fixture keys: '+JSON.stringify(fixture.keys.map(k=>[k.id,k.provider_id,k.enabled,k.allowed_models])));
        console.error('DIAG fixture models: '+JSON.stringify(fixture.config.models.map(m=>[m.id,m.exposed_model_id,m.visible,m.retired,m.target_provider_id,m.target_model])));
        console.error('DIAG fixture providers: '+JSON.stringify(fixture.providers.map(p=>[p.id,p.enabled])));
        console.error('DIAG requests: '+JSON.stringify(diagRequests.slice(-30)));
        console.error('DIAG console: '+JSON.stringify(diagConsole.slice(-20)));
      }
      if(option!==undefined)await box.getByRole('checkbox').setChecked(option);
      await (accept?box.locator('[data-confirm="accept"]'):box.getByRole('button',{name:'取消',exact:true})).click();
      await box.waitFor({state:'detached'});return text;
    };
    // gpt-5 loses its only route; gemini-pro loses its primary but has a backup on the OpenAI provider.
    key('fixture-key').allowed_models=['claude-sonnet','gpt-6-astra'];
    model('fixture-model-2').fallback_chain=[{provider_id:'fixture-openai',target_model:'gpt-6-astra'}];
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();
    await button('刷新').waitFor();
    const down=page.getByRole('alert').filter({hasText:'个在售模型无可用线路'});
    assert.equal((await down.innerText()).replace(/\s+/g,' ').includes('1 个在售模型无可用线路，客户请求会失败：gpt-5（测试供应商 / Fixture 没有启用的 Key 授权 gpt-5）'),true,await down.innerText());
    await page.getByRole('status').filter({hasText:'1 个在售模型的主线路不可用，正由备用线路服务：gemini-pro'}).waitFor();
    assert.equal(await page.locator('#nav-badge-models [aria-hidden="true"]').textContent(),'2');
    await down.getByRole('button',{name:'去模型与定价',exact:true}).click();
    const row=name=>page.getByRole('row').filter({has:page.getByRole('button',{name:'编辑',exact:true})}).filter({hasText:name});
    await row('gpt-5').getByText('无可用线路',{exact:true}).waitFor();
    await row('gemini-pro').getByText('主线路不可用',{exact:true}).waitFor();
    assert.equal(await row('claude-sonnet').locator('.route-tags').count(),0,'a model that can be served carries no route warning');
    // The upstream hint ignores disabled Keys and disabled providers.
    await row('gpt-5').getByRole('button',{name:'编辑',exact:true}).click();
    const editor=page.locator('.mapping-editor');
    await editor.getByText('这个供应商的 Key 还没有授权此模型').waitFor();
    await editor.getByLabel('供应商',{exact:true}).selectOption('fixture-disabled');
    await editor.getByText('这个供应商已停用').waitFor();
    await editor.getByLabel('供应商',{exact:true}).selectOption('fixture-provider');
    assert.equal(await page.getByRole('region',{name:'发布'}).count(),0,'back to the original: nothing to publish');
    console.log('PASS: models without a route named on 运营概览 and marked on their rows; backup takeover told apart; upstream hint ignores disabled Keys and providers');

    // 停用供应商: the confirmation names what each shown model loses and can hide the ones left with nothing.
    key('fixture-key').allowed_models=['claude-sonnet','gpt-5','gemini-pro','gpt-6-astra'];
    await nav('供应商与 Key');await refresh();
    const toggle=page.getByRole('switch',{name:'启用 测试供应商 / Fixture',exact:true});
    let refusals=1;
    await page.route('**/api/v1/admin/commercial-config',route=>route.request().method()==='POST'&&refusals-->0
      ?route.fulfill({status:409,json:{success:false,error:'Invalid billing state: Configuration changed; reload before publishing'}}):route.continue());
    await toggle.click();
    const facts=await confirm({option:true});
    for(const expected of ['将无可用线路（客户请求会失败）：claude-sonnet、gpt-5','主线路失效，改由备用线路服务：gemini-pro','同时隐藏将无可用线路的 2 个模型（先隐藏，再停用）'])
      assert(facts.includes(expected),`${expected}\n${facts}`);
    await page.getByRole('alert').filter({hasText:'没能隐藏这些模型：配置刚被更新'}).filter({hasText:'测试供应商 / Fixture 没有停用'}).waitFor();
    assert.equal(writes('providers/status').length,0,'a refused hide leaves the provider as it was');
    assert.equal(await toggle.getAttribute('aria-checked'),'true');
    await button('关闭提示').click();
    const revision=fixture.config.revision;
    await toggle.click();await confirm({option:true});
    await page.locator('.toast').filter({hasText:'已隐藏 claude-sonnet、gpt-5，并停用 测试供应商 / Fixture'}).waitFor();
    const published=writes('commercial-config').at(-1).body;
    assert.equal(published.expected_revision,revision);assert(published.reason.includes('停用 测试供应商 / Fixture'),published.reason);
    assert.deepEqual(published.models.map(row=>[row.id,row.visible]),[['fixture-model-0',false],['fixture-model-1',false]]);
    const order=fixture.writes.map(write=>write.endpoint);
    assert(order.lastIndexOf('commercial-config')<order.lastIndexOf('providers/status'),'hidden first, then disabled');
    assert.deepEqual(writes('providers/status').at(-1).body,{providerId:'fixture-provider',enabled:false});
    await toggle.click();await confirm();await page.locator('.toast').filter({hasText:'已启用 测试供应商 / Fixture'}).waitFor();
    console.log('PASS: 停用供应商 lists losses by kind; 同时隐藏 publishes the hidden models first, revision-checked; a refused hide changes nothing');

    // A Key's save: the models it would strand are listed and can be hidden in the same step.
    for(const id of ['fixture-model-0','fixture-model-1'])model(id).visible=true;
    fixture.config.revision='fixture-rev-20';await refresh();
    const openKey=async(provider,id)=>{await page.locator('.provider-card').filter({hasText:provider}).getByRole('row').filter({hasText:id}).getByRole('button',{name:'编辑',exact:true}).click();await page.locator('#key-editor').waitFor();};
    await openKey('OpenAI 格式 / Fixture','fixture-openai-key-1');
    await page.getByRole('group',{name:'可用模型'}).getByRole('checkbox',{name:'gpt-6-astra',exact:true}).uncheck();
    await button('保存').click();
    const saveFacts=await confirm({option:true});
    for(const expected of ['将无可用线路（客户请求会失败）：gpt-6-astra','少一条备用线路（仍可服务）：gemini-pro','同时隐藏将无可用线路的 1 个模型（先隐藏，再保存）'])
      assert(saveFacts.includes(expected),`${expected}\n${saveFacts}`);
    await page.locator('.toast').filter({hasText:'已隐藏 gpt-6-astra，并保存 Key fixture-openai-key-1'}).waitFor();
    assert.deepEqual(writes('commercial-config').at(-1).body.models.map(row=>[row.id,row.visible]),[['fixture-model-3',false]]);
    assert.deepEqual(writes('providers/keys').at(-1).body.allowed_models,['gpt-5.6-sol','gpt-5.6-terra']);
    console.log('PASS: Key save lists stranded models and backups lost; 同时隐藏 hides them before saving');

    // A Key's delete: nothing lost goes straight through; a Key that is a shown model's only route is
    // refused by the server unless the model is hidden with it.
    await openKey('测试供应商 / Fixture','fixture-backup');
    await button('删除 Key').click();
    const plain=await confirm();assert(!plain.includes('同时隐藏'),plain);
    await page.locator('.toast').filter({hasText:'已删除 Key fixture-backup'}).waitFor();
    await page.locator('#key-editor').waitFor({state:'detached'});
    assert.equal(key('fixture-backup'),undefined);
    model('fixture-model-3').visible=true;key('fixture-openai-key-1').allowed_models=['gpt-6-astra'];fixture.config.revision='fixture-rev-30';await refresh();
    await openKey('OpenAI 格式 / Fixture','fixture-openai-key-1');
    await button('删除 Key').click();
    const deleteFacts=await confirm({option:false});
    assert(deleteFacts.includes('将无可用线路（客户请求会失败）：gpt-6-astra')&&deleteFacts.includes('服务器会拒绝删除'),deleteFacts);
    await page.getByRole('status').filter({hasText:'删除失败：这个 Key 还在为在售模型服务：gpt-6-astra'}).waitFor();
    assert(await button('删除 Key').isEnabled(),'a refusal does not lock the editor');
    assert.equal(writes('providers/keys/delete').length,2);assert(key('fixture-openai-key-1'));
    await button('删除 Key').click();
    const box=page.getByRole('alertdialog');await box.waitFor();assert(await box.getByRole('checkbox').isChecked(),'hiding is ticked when the delete needs it');
    await box.locator('[data-confirm="accept"]').click();
    await page.locator('.toast').filter({hasText:'已隐藏 gpt-6-astra，并删除 Key fixture-openai-key-1'}).waitFor();
    assert.equal(key('fixture-openai-key-1'),undefined);assert.equal(model('fixture-model-3').visible,false);
    // Hidden on the way, then the delete is not confirmed: the message says the model stays hidden.
    model('fixture-model-3').visible=true;fixture.keys.push({id:'fixture-openai-key-2',provider_id:'fixture-openai',allowed_models:['gpt-6-astra'],weight:1,enabled:true,health_state:'healthy'});
    fixture.config.revision='fixture-rev-40';await refresh();
    await page.route('**/api/v1/admin/providers/keys/delete',route=>route.fulfill({status:503,json:{success:false,error:'Provider persistence failed'}}),{times:1});
    await openKey('OpenAI 格式 / Fixture','fixture-openai-key-2');
    await button('删除 Key').click();await confirm();
    await page.getByRole('status').filter({hasText:'已隐藏 gpt-6-astra；但没收到删除结果（Provider persistence failed）'}).waitFor();
    assert.equal(model('fixture-model-3').visible,false);assert(key('fixture-openai-key-2'));
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log('PASS: 删除 Key: no-loss delete, server refusal explained with the model named, hide-then-delete; a failure after hiding says the model stays hidden');
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
