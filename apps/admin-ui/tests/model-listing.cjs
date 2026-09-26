// 上架模型 regression: final build + loopback fixture, never production.
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
(async()=>{
  let browser;
  try{
    await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
    browser=await chromium.launch({headless:true,...(process.env.CHROME_PATH?{executablePath:process.env.CHROME_PATH}:{})});
    const page=await browser.newPage({viewport:{width:1280,height:900}}),errors=[];
    page.setDefaultTimeout(10000);
    const origin=`http://127.0.0.1:${server.address().port}`;
    page.on('pageerror',error=>{errors.push(error.message);console.error('Browser error:',error.message);});
    await page.route('**/*',route=>new URL(route.request().url()).origin===origin?route.continue():route.abort());
    const button=name=>page.getByRole('button',{name,exact:true});
    const nav=name=>page.getByRole('navigation').getByRole('button',{name,exact:true}).click();
    const answer=async accept=>{
      const box=page.getByRole('alertdialog');await box.waitFor();
      await (accept?box.locator('[data-confirm="accept"]'):box.getByRole('button',{name:'取消',exact:true})).click();
      await box.waitFor({state:'detached'});
    };
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();
    await button('刷新').waitFor();
    // The configuration the console reads and publishes to, with a list order; every publication is kept.
    let config=(await (await page.request.get(origin+'/api/v1/admin/commercial-config')).json()).config;
    config={...config,models:config.models.map((model,index)=>({...model,sort_order:index,aliases:[],fallback_chain:[]}))};
    const posts=[];
    await page.route('**/api/v1/admin/commercial-config',async route=>{
      if(route.request().method()==='GET')return route.fulfill({json:{success:true,config}});
      const body=route.request().postDataJSON();posts.push(body);
      config={...config,models:[...config.models.filter(row=>!(body.models||[]).some(next=>next.id===row.id)),...(body.models||[])],
        versions:[...config.versions,...(body.versions||[])],revision:`fixture-listing-${posts.length}`};
      return route.fulfill({json:{success:true,config}});
    });

    await nav('模型与定价');
    await button('＋ 上架模型').click();
    const drawer=page.locator('#listing-drawer');await drawer.waitFor();
    await drawer.getByLabel('供应商',{exact:true}).selectOption('fixture-openai');
    // A model no enabled Key authorises is pointed out; the model ID follows the upstream model.
    const upstream=drawer.getByLabel('上游模型',{exact:true});
    await upstream.fill('not-authorised');await drawer.getByText('的 Key 还没有授权这个模型').waitFor();
    await upstream.fill('gpt-5.6-sol');
    const modelId=drawer.getByLabel('模型 ID',{exact:true});
    assert.equal(await modelId.inputValue(),'gpt-5.6-sol');
    assert.equal(await drawer.getByLabel('显示名称',{exact:true}).getAttribute('placeholder'),'留空显示为 GPT 5.6 Sol');
    // 参照现有模型 copies capabilities, display multiplier and price, and places it after the reference.
    await drawer.getByLabel('参照现有模型',{exact:true}).selectOption('fixture-model-3');
    assert.equal(await drawer.getByLabel('上下文长度',{exact:true}).inputValue(),'272000');
    assert.equal(await drawer.getByLabel('输入售价',{exact:true}).inputValue(),'1.25');
    assert.equal(await drawer.getByLabel('位置',{exact:true}).inputValue(),'fixture-model-3');
    await drawer.getByLabel('位置',{exact:true}).selectOption('fixture-model-0');
    // 按官方价计算: official USD x the multipliers at the face value (the fixture's credit is CNY 0.01).
    await drawer.getByText('按官方价计算',{exact:true}).click();
    for(const [label,value] of [['官方输入价','4'],['官方输出价','20'],['官方缓存写价','5'],['官方缓存读价','0.4']])await drawer.getByLabel(label,{exact:true}).fill(value);
    await drawer.getByLabel('售价倍率',{exact:true}).fill('0.24');await drawer.getByLabel('成本倍率',{exact:true}).fill('0.06');
    await drawer.getByRole('button',{name:'计算',exact:true}).click();
    assert.equal(await drawer.getByLabel('输入售价',{exact:true}).inputValue(),'96');
    assert.equal(await drawer.getByLabel('缓存读售价',{exact:true}).inputValue(),'9.6');
    assert.equal(await drawer.getByLabel('采购输出价',{exact:true}).inputValue(),'1.2');
    // A model ID the group already has is refused before anything is sent.
    const list=drawer.getByRole('button',{name:'上架',exact:true});
    await modelId.fill('gpt-6-astra');await list.click();
    await drawer.getByRole('alert').filter({hasText:'已经有 gpt-6-astra'}).waitFor();
    assert.equal(posts.length,0);
    await modelId.fill('gpt-5.6-sol');
    await list.click();
    const box=page.getByRole('alertdialog');await box.waitFor();const facts=await box.innerText();
    for(const expected of ['客户看到：GPT 5.6 Sol（gpt-5.6-sol）','排在 claude-sonnet 之后','线路：OpenAI 格式 / Fixture / gpt-5.6-sol','售价：输入 96 / 输出 480 / 缓存写 120 / 缓存读 9.6 积分/百万'])
      assert(facts.includes(expected),`${expected}\n${facts}`);
    await answer(true);
    // Step one: the model, hidden, with its first price about 20 seconds ahead.
    await drawer.getByRole('status').filter({hasText:'秒后价格生效'}).waitFor();
    assert.equal(posts.length,1);
    const first=posts[0];
    assert.deepEqual(Object.keys(first).sort(),['expected_revision','models','reason','versions']);
    assert.equal(first.reason,'上架 gpt-5.6-sol（OpenAI 格式 / Fixture）');
    const [mapping]=first.models,[version]=first.versions;
    for(const [field,value] of Object.entries({id:'fixture-openai-gpt-5.6-sol',visible:false,group_id:'fixture-group-0',target_provider_id:'fixture-openai',target_model:'gpt-5.6-sol',sort_order:1,context_window:272000,credit_multiplier:1}))
      assert.equal(mapping[field],value,field);
    for(const [field,value] of Object.entries({model:'gpt-5.6-sol',rate_card_id:'fixture-rate',pricing_mode:'fixed',currency:'CNY',fixed_input_credit_per_m:96000000,fixed_cache_read_credit_per_m:9600000,output_price_per_m:1.2,margin_multiplier:1}))
      assert.equal(version[field],value,field);
    const lead=version.effective_from_secs-Date.now()/1000;assert(lead>12&&lead<=21,`the price starts about 20 s later (${lead})`);
    // Step two, once the price is in force: shown, and the model after it in its group moved down one.
    await page.locator('.toast').filter({hasText:'已上架 gpt-5.6-sol'}).waitFor({timeout:40000});
    assert.equal(posts.length,2);
    const second=posts[1];
    assert.deepEqual(Object.keys(second).sort(),['expected_revision','models','reason']);
    assert.equal(second.expected_revision,'fixture-listing-1');
    assert.deepEqual(second.models.map(row=>[row.id,row.sort_order,row.visible]).sort(),[['fixture-model-3',4,true],['fixture-openai-gpt-5.6-sol',1,true]]);
    await drawer.waitFor({state:'detached'});
    await page.getByRole('row').filter({hasText:'gpt-5.6-sol'}).first().waitFor();
    console.log('PASS: 上架模型: authorised upstream models, reference copy, official price x multipliers, duplicate refused, hidden with price then shown in place');

    // From 供应商与 Key: a saved, unlisted model opens 上架模型 already filled in.
    await nav('供应商与 Key');
    await page.locator('.provider-card').filter({hasText:'OpenAI 格式 / Fixture'}).getByRole('button',{name:'编辑',exact:true}).click();
    const picker=page.getByRole('group',{name:'可用模型'});
    const terra=picker.locator('li').filter({hasText:'gpt-5.6-terra'});
    await terra.getByText('未上架',{exact:true}).waitFor();
    assert.equal(await picker.locator('li').filter({hasText:'gpt-5.6-sol'}).getByText('未上架',{exact:true}).count(),0,'the model just listed is no longer marked');
    await terra.getByRole('button',{name:'去上架',exact:true}).click();
    const again=page.locator('#listing-drawer');await again.waitFor();
    assert.equal(await page.getByRole('navigation').getByRole('button',{name:'模型与定价',exact:true}).getAttribute('aria-current'),'page');
    assert.equal(await again.getByLabel('供应商',{exact:true}).inputValue(),'fixture-openai');
    assert.equal(await again.getByLabel('上游模型',{exact:true}).inputValue(),'gpt-5.6-terra');
    await again.getByRole('button',{name:'取消',exact:true}).click();await again.waitFor({state:'detached'});
    assert.equal(posts.length,2,'opening and cancelling publishes nothing');
    assert.deepEqual(errors,[]);
    console.log('PASS: 去上架 from a Key opens the listing for that provider and model');
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
