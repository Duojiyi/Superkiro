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
      const box=page.getByRole('alertdialog');await box.waitFor();const text=await box.innerText();
      await (accept?box.locator('[data-confirm="accept"]'):box.getByRole('button',{name:'取消',exact:true})).click();
      await box.waitFor({state:'detached'});return text;
    };
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();
    await button('刷新').waitFor();
    // The configuration the console reads and publishes to, with a list order; every publication is kept.
    let config=(await (await page.request.get(origin+'/api/v1/admin/commercial-config')).json()).config;
    config={...config,models:config.models.map((model,index)=>({...model,sort_order:index}))};
    const posts=[];let lose=false;
    await page.route('**/api/v1/admin/commercial-config',async route=>{
      if(route.request().method()==='GET')return route.fulfill({json:{success:true,config}});
      const body=route.request().postDataJSON();posts.push(body);
      if(lose)return route.abort('failed');
      config={...config,models:[...config.models.filter(row=>!(body.models||[]).some(next=>next.id===row.id)),...(body.models||[])],
        versions:[...config.versions,...(body.versions||[]).map(version=>({...version,effective_from_secs:version.effective_from_secs||Math.floor(Date.now()/1000)}))],revision:`fixture-listing-${posts.length}`};
      return route.fulfill({json:{success:true,config}});
    });

    await nav('模型与定价');
    await button('＋ 上架模型').click();
    const drawer=page.locator('#listing-drawer');await drawer.waitFor();
    await drawer.getByLabel('供应商',{exact:true}).selectOption('fixture-openai');
    // A model no enabled Key authorises is pointed out; the model ID follows the upstream model.
    const upstream=drawer.getByLabel('上游模型',{exact:true});
    await upstream.fill('not-authorised');await drawer.getByText('的 Key 还没有授权这个模型').waitFor();
    assert(await drawer.getByRole('button',{name:'测试',exact:true}).isDisabled(),'nothing to test on a route no Key allows');
    await upstream.fill('gpt-5.6-sol');
    // 测试 before listing: one tiny real request through this provider, nothing saved.
    await drawer.getByRole('button',{name:'测试',exact:true}).click();
    await drawer.getByRole('status').filter({hasText:'成功 · 首字 410 ms'}).waitFor();
    assert.deepEqual(fixture.writes.filter(write=>write.endpoint==='providers/keys/probe').at(-1).body,{provider_id:'fixture-openai',model:'gpt-5.6-sol'});
    const modelId=drawer.getByLabel('模型 ID',{exact:true});
    assert.equal(await modelId.inputValue(),'gpt-5.6-sol');
    assert.equal(await drawer.getByLabel('显示名称',{exact:true}).getAttribute('placeholder'),'留空显示为 GPT 5.6 Sol');
    // 参照现有模型 copies capabilities, the model and display multipliers and price, and places it after the reference.
    await drawer.getByLabel('参照现有模型',{exact:true}).selectOption('fixture-model-3');
    assert.equal(await drawer.getByLabel('上下文长度',{exact:true}).inputValue(),'272000');
    assert.equal(await drawer.getByLabel('输入售价',{exact:true}).inputValue(),'1.25');
    assert.equal(await drawer.getByLabel('模型扣费倍率',{exact:true}).inputValue(),'1');
    const place=drawer.getByLabel('位置',{exact:true});
    assert.equal(await place.inputValue(),'fixture-model-3');
    assert((await place.locator('option').allTextContents()).includes('排在最前（成为默认模型）'));
    await place.selectOption('fixture-model-0');
    // 按官方价计算: official USD x the multipliers at the face value (the fixture's credit is CNY 0.01).
    await drawer.getByText('按官方价计算',{exact:true}).click();
    for(const [label,value] of [['官方输入价','4'],['官方输出价','20'],['官方缓存写价','5'],['官方缓存读价','0.4']])await drawer.getByLabel(label,{exact:true}).fill(value);
    await drawer.getByLabel('售价倍率',{exact:true}).fill('0.24');await drawer.getByLabel('成本倍率',{exact:true}).fill('0.06');
    await drawer.getByRole('button',{name:'计算',exact:true}).click();
    assert.equal(await drawer.getByLabel('输入售价',{exact:true}).inputValue(),'96');
    assert.equal(await drawer.getByLabel('缓存读售价',{exact:true}).inputValue(),'9.6');
    assert.equal(await drawer.getByLabel('采购输出价',{exact:true}).inputValue(),'1.2');
    // Each upstream keeps its own cost multiplier; the retail one is shared.
    assert.deepEqual(await page.evaluate(()=>JSON.parse(localStorage.getItem('admin-listing-rates:v2'))),{retail:'0.24',upstream:{'fixture-openai':'0.06'}});
    await drawer.getByLabel('供应商',{exact:true}).selectOption('fixture-provider');
    assert.equal(await drawer.getByLabel('成本倍率',{exact:true}).inputValue(),'');assert.equal(await drawer.getByLabel('售价倍率',{exact:true}).inputValue(),'0.24');
    await drawer.getByLabel('供应商',{exact:true}).selectOption('fixture-openai');
    assert.equal(await drawer.getByLabel('成本倍率',{exact:true}).inputValue(),'0.06');
    // Model IDs the server would refuse are stopped here; so is one the group already has.
    const list=drawer.getByRole('button',{name:'上架',exact:true});
    await modelId.fill('gpt 5.6 sol');await drawer.getByText('模型 ID 只能用英文字母、数字和 . _ : / -，最多 128 个字符').waitFor();
    assert(await list.isDisabled());
    await modelId.fill('gpt-6-astra');await list.click();
    await drawer.getByRole('alert').filter({hasText:'已经有 gpt-6-astra'}).waitFor();
    assert.equal(posts.length,0);
    await modelId.fill('gpt-5.6-sol');
    await list.click();
    const facts=await answer(true);
    for(const expected of ['客户看到：GPT 5.6 Sol（gpt-5.6-sol）','排在 claude-sonnet 之后','线路：OpenAI 格式 / Fixture / gpt-5.6-sol','售价：输入 96 / 输出 480 / 缓存写 120 / 缓存读 9.6 积分/百万 · 上架即生效','扣费倍率 1'])
      assert(facts.includes(expected),`${expected}\n${facts}`);
    // One publication: shown at once with its first price in force now, the group numbered again from its place.
    await page.locator('.toast').filter({hasText:'已上架 gpt-5.6-sol'}).waitFor();
    await drawer.waitFor({state:'detached'});
    assert.equal(posts.length,1);
    const first=posts[0];
    assert.deepEqual(Object.keys(first).sort(),['expected_revision','models','reason','versions']);
    assert.equal(first.reason,'上架 gpt-5.6-sol（OpenAI 格式 / Fixture）');
    assert.deepEqual(first.models.map(row=>[row.id,row.sort_order,row.visible]),[['fixture-model-3',2,true],['fixture-openai-gpt-5.6-sol',1,true]]);
    const mapping=first.models.at(-1),[version]=first.versions;
    for(const [field,value] of Object.entries({group_id:'fixture-group-0',target_provider_id:'fixture-openai',target_model:'gpt-5.6-sol',context_window:272000,credit_multiplier:1}))
      assert.equal(mapping[field],value,field);
    for(const [field,value] of Object.entries({model:'gpt-5.6-sol',rate_card_id:'fixture-rate',pricing_mode:'fixed',currency:'CNY',fixed_input_credit_per_m:96000000,fixed_cache_read_credit_per_m:9600000,output_price_per_m:1.2,margin_multiplier:1,effective_from_secs:0}))
      assert.equal(version[field],value,field);
    await page.getByRole('row').filter({hasText:'gpt-5.6-sol'}).first().waitFor();
    console.log('PASS: 上架模型: 测试 first, reference copy with the model multiplier, per-provider cost multiplier, ID rule, one publication shown with its price, renumbered place');

    // 去上架 while the page has unpublished edits: it says so instead of doing nothing.
    await page.getByLabel('上下文长度',{exact:true}).fill('150000');
    await nav('供应商与 Key');await answer(true);
    await page.locator('.provider-card').filter({hasText:'OpenAI 格式 / Fixture'}).getByRole('button',{name:'编辑',exact:true}).click();
    const picker=page.getByRole('group',{name:'可用模型'});
    const terra=picker.locator('li').filter({hasText:'gpt-5.6-terra'});
    await terra.getByText('未上架',{exact:true}).waitFor();
    assert.equal(await picker.locator('li').filter({hasText:'gpt-5.6-sol'}).getByText('未上架',{exact:true}).count(),0,'the model just listed is no longer marked');
    await terra.getByRole('button',{name:'去上架',exact:true}).click();
    await page.getByRole('status').filter({hasText:'没有打开“上架 gpt-5.6-terra”：先发布或放弃未发布的修改，再上架'}).waitFor();
    assert.equal(await page.locator('#listing-drawer').count(),0);
    await button('放弃修改').click();await answer(true);
    await page.waitForFunction(()=>![...document.querySelectorAll('button')].some(b=>b.textContent==='放弃修改'));
    await nav('供应商与 Key');
    await page.locator('.provider-card').filter({hasText:'OpenAI 格式 / Fixture'}).getByRole('button',{name:'编辑',exact:true}).click();
    await page.getByRole('group',{name:'可用模型'}).locator('li').filter({hasText:'gpt-5.6-terra'}).getByRole('button',{name:'去上架',exact:true}).click();
    const again=page.locator('#listing-drawer');await again.waitFor();
    assert.equal(await page.getByRole('navigation').getByRole('button',{name:'模型与定价',exact:true}).getAttribute('aria-current'),'page');
    assert.equal(await again.getByLabel('供应商',{exact:true}).inputValue(),'fixture-openai');
    assert.equal(await again.getByLabel('上游模型',{exact:true}).inputValue(),'gpt-5.6-terra');
    console.log('PASS: 去上架 from a Key opens the listing for that provider and model, and says why when the page has unpublished edits');

    // A model ID the shared price table already prices (PRO+ sells gpt-5): its price is kept by
    // default; a new one would start later and apply to PRO+ as well.
    await again.getByLabel('模型 ID',{exact:true}).fill('gpt-5');
    await again.getByText('价格表里已有 gpt-5 的价格：输入 3 / 输出 15 / 缓存写 3.75 / 缓存读 0.3 积分/百万，PRO+ 分组的 gpt-5 按它扣费').waitFor();
    assert.equal(await again.getByRole('radio',{name:'沿用现有价格',exact:true}).getAttribute('aria-checked'),'true');
    assert.equal(await again.getByLabel('输入售价',{exact:true}).count(),0,'no price to type when the existing one is kept');
    await again.getByRole('radio',{name:'设新价格',exact:true}).click();
    await again.getByText('新价格在发布后 5 分钟生效，在那之前按现有价格扣费；它也会用于 PRO+ 分组的 gpt-5').waitFor();
    for(const [label,value] of [['输入售价','4'],['输出售价','16'],['缓存写售价','4'],['缓存读售价','0.4'],['采购输入价','1'],['采购输出价','1'],['采购缓存写价','1'],['采购缓存读价','1']])await again.getByLabel(label,{exact:true}).fill(value);
    await again.getByRole('button',{name:'上架',exact:true}).click();
    const newPrice=await answer(false);
    assert(newPrice.includes('新价格同时用于 PRO+ 分组的 gpt-5（同一价格表）')&&/在那之前按现有价格/.test(newPrice),newPrice);
    await again.getByRole('radio',{name:'沿用现有价格',exact:true}).click();
    // A result that never arrives: the drawer closes and the page says exactly what to check.
    lose=true;
    await again.getByRole('button',{name:'上架',exact:true}).click();
    assert((await answer(true)).includes('售价：沿用价格表里现有的价格（输入 3 / 输出 15'));
    await page.getByRole('status').filter({hasText:'没收到上架结果'}).filter({hasText:'请点“重新加载”后核对“模型与定价”里有没有 gpt-5（PRO）、“价格版本”里有没有它的价格，不要重复提交'}).waitFor();
    await again.waitFor({state:'detached'});
    const kept=posts.at(-1);assert.equal('versions' in kept,false,'the existing price is kept: no version sent');
    assert.deepEqual(kept.models.map(row=>[row.exposed_model_id,row.visible]),[['gpt-5',true]]);
    assert(await button('＋ 上架模型').isDisabled(),'nothing else is published before the result is checked');
    assert.deepEqual(errors,[]);
    console.log('PASS: a shared price table: existing price kept by default, a new one scheduled and naming the other groups; an unconfirmed listing says what to check');
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
