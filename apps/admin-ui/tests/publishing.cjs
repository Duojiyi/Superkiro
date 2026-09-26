// Publication refusals and conflicts: final build + loopback fixture, never production.
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
const model=id=>fixture.config.models.find(row=>row.id===id);
const published=()=>fixture.writes.filter(write=>write.endpoint==='commercial-config');
// Someone else publishes: the fixture's configuration moves on to a new version.
const elsewhere=change=>{change();fixture.config.revision=`fixture-rev-${Number(fixture.config.revision.split('-').pop())+10}`;};
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
    const accept=async()=>{const box=page.getByRole('alertdialog');await box.waitFor();await box.locator('[data-confirm="accept"]').click();await box.waitFor({state:'detached'});};
    const bar=page.getByRole('region',{name:'发布'});
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();
    await button('刷新').waitFor();
    await nav('模型与定价');
    const editor=page.locator('.mapping-editor'),field=name=>editor.getByLabel(name,{exact:true});
    await field('上下文长度').waitFor();
    // The owner edits claude-sonnet while someone else changes its context and its output limit.
    await field('上下文长度').fill('1000000');await field('显示名称').fill('Sonnet');
    elsewhere(()=>{Object.assign(model('fixture-model-0'),{context_window:300000,max_output:16384});});
    await bar.getByLabel('变更原因',{exact:true}).fill('上下文和显示名');
    await button('发布').click();await accept();
    await bar.getByRole('status').filter({hasText:'配置刚被别人更新（或在另一个窗口发布过），这次什么都没有发布'}).waitFor();
    assert.equal(await bar.getByText('没收到发布结果').count(),0,'a refusal is not an unconfirmed publication');
    assert(await button('发布').isEnabled(),'the draft stays editable and publishable');
    assert.equal(await field('显示名称').inputValue(),'Sonnet');
    await button('重新加载并保留修改').click();
    await bar.getByRole('status').filter({hasText:'保留了 1 项修改；这些修改没有套用，因为别人已经改过：claude-sonnet 的上下文（服务器现为 300K）'}).waitFor();
    assert.equal(await field('显示名称').inputValue(),'Sonnet','an edit to a field nobody else touched is kept');
    assert.equal(await field('上下文长度').inputValue(),'300000','a field changed on the server keeps the server value');
    assert.equal(await field('最大输出').inputValue(),'16384');
    assert.equal(await bar.getByLabel('变更原因',{exact:true}).inputValue(),'上下文和显示名','the reason is kept');
    await button('发布').click();await accept();
    await page.locator('.toast').filter({hasText:'已发布'}).waitFor();
    const sent=published().at(-1).body;
    assert.equal(sent.expected_revision,'fixture-rev-12');
    assert.deepEqual(sent.models.map(row=>[row.id,row.display_name,row.context_window,row.max_output]),[['fixture-model-0','Sonnet',300000,16384]],'only the model that changed is sent');
    assert.equal('rate_cards' in sent,false);
    console.log('PASS: a conflict keeps the draft; 重新加载并保留修改 reapplies untouched fields, reports the rest, keeps the reason; only changed models are sent');

    // A model without a route elsewhere does not block publishing another; a refusal is explained and nothing is locked.
    fixture.keys.find(row=>row.id==='fixture-key').allowed_models=['claude-sonnet','gemini-pro','gpt-6-astra'];
    await button('刷新').click();await page.locator('.btn-refresh:not([disabled])').waitFor();
    await field('最大输出').fill('32000');await bar.getByLabel('变更原因',{exact:true}).fill('输出上限');
    let refusal=true;
    await page.route('**/api/v1/admin/commercial-config',route=>route.request().method()==='POST'&&refusal
      ?(refusal=false,route.fulfill({status:409,json:{success:false,error:'Invalid billing state: Margins and model multipliers combine to more than 100x'}})):route.continue());
    await button('发布').click();await accept();
    await bar.getByRole('status').filter({hasText:'服务器拒绝了这次发布：版本倍率 × 分组倍率 × 模型倍率超过了 100 倍'}).waitFor();
    assert(await button('发布').isEnabled());assert.equal(await field('最大输出').inputValue(),'32000');
    await button('发布').click();await accept();
    await page.locator('.toast').filter({hasText:'已发布'}).waitFor();
    assert.deepEqual(published().at(-1).body.models.map(row=>row.id),['fixture-model-0'],'gpt-5, now without a route, is not part of this publication');
    console.log('PASS: refusals are explained in plain words and leave the draft editable; an unrelated model without a route does not block a publication');

    // Shown to customers only with a price in force and a primary route that serves: refused before sending otherwise.
    fixture.keys.find(row=>row.id==='fixture-key').allowed_models=['claude-sonnet','gpt-5','gemini-pro','gpt-6-astra'];
    elsewhere(()=>fixture.config.models.push({...model('fixture-model-3'),id:'fixture-model-9',exposed_model_id:'unpriced-model',target_model:'gpt-5.6-sol',visible:false,sort_order:5}));
    await button('刷新').click();await page.locator('.btn-refresh:not([disabled])').waitFor();
    const unpriced=page.getByRole('row').filter({has:page.getByRole('button',{name:'unpriced-model 的更多操作',exact:true})});
    await unpriced.getByRole('button',{name:'编辑',exact:true}).click();
    await editor.getByRole('checkbox',{name:'客户可见',exact:true}).check();
    await bar.getByLabel('变更原因',{exact:true}).fill('对客户开放');
    const count=published().length;
    await button('发布').click();
    await bar.getByRole('status').filter({hasText:'这些模型还没有生效中的价格，不能对客户显示：unpriced-model。先给它们调价，或保持隐藏'}).waitFor();
    assert.equal(await page.getByRole('alertdialog').count(),0);assert.equal(published().length,count);
    // Priced meanwhile; then its route loses its Key.
    elsewhere(()=>fixture.config.versions.push({...fixture.config.versions.find(version=>version.id==='fixture-price-astra'),id:'unpriced-model-price',model:'unpriced-model'}));
    fixture.keys.find(row=>row.id==='fixture-openai-key-1').allowed_models=['gpt-6-astra'];
    await button('刷新').click();await bar.getByText('服务器上的配置可能已更新').waitFor();
    await button('重新加载并保留修改').click();await bar.getByRole('status').filter({hasText:'保留了你的 1 项修改'}).waitFor();
    await button('发布').click();
    await bar.getByRole('status').filter({hasText:'这些在售模型的主线路不能用：unpriced-model（OpenAI 格式 / Fixture 没有启用的 Key 授权 gpt-5.6-sol）'}).waitFor();
    assert.equal(published().length,count);
    fixture.keys.find(row=>row.id==='fixture-openai-key-1').allowed_models=['gpt-6-astra','gpt-5.6-sol'];
    await button('刷新').click();await page.locator('.btn-refresh:not([disabled])').waitFor();
    await button('发布').click();await accept();await page.locator('.toast').filter({hasText:'已发布'}).waitFor();
    assert.deepEqual(published().at(-1).body.models.map(row=>[row.id,row.visible]),[['fixture-model-9',true]]);
    console.log('PASS: a model is shown only with a price in force and a primary route that serves; each refusal names the model before anything is sent');

    // 结算参数: the same refusal and reload for the face value and exchange rate.
    await nav('财务对账');
    const face=page.getByLabel('积分面值',{exact:true}),reason=page.getByLabel('变更原因',{exact:true});
    await face.waitFor();await page.waitForFunction(()=>!document.querySelector('.settings-panel fieldset')?.disabled);
    await face.fill('0.03');await reason.fill('新面值');
    elsewhere(()=>{});
    await button('发布').click();await accept();
    await page.getByRole('status').filter({hasText:'配置刚被别人更新'}).waitFor();
    assert.equal(await face.inputValue(),'0.03');assert.equal(await page.getByText('没收到发布结果').count(),0);
    await button('重新加载并保留修改').click();
    await page.getByRole('status').filter({hasText:'你填的数值和原因都保留了'}).waitFor();
    assert.equal(await face.inputValue(),'0.03');assert.equal(await reason.inputValue(),'新面值');
    await button('发布').click();await accept();
    await page.locator('.toast').filter({hasText:'已发布结算参数'}).waitFor();
    assert.equal(fixture.config.settings.credit_face_value_cny,0.03);
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log('PASS: 结算参数: a conflict keeps the typed values and reason; reloaded, the same values publish');
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
