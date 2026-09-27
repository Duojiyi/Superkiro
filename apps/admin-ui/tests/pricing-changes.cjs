// 调价, 批量调价 and 上架 from official prices: final build + loopback fixture (seeded with official
// prices), never production. Every publication goes through the fixture's copy of the server's rules.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
const http=require('node:http'),fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict');
const fixture=require('./fixture-api.cjs')();
const {promo,official,plain}=require('./pricing-seed.cjs')(fixture);
const root=path.resolve(__dirname,'../dist');
const now=Math.floor(Date.now()/1000);
// An earlier official price of gpt-5 (计费倍率 0.3), for 恢复上一版价格.
fixture.config.versions.push(plain(official.officialVersion({official:[4,20,5,0.4],priceMultiplier:0.3,costMultiplier:0.08,basis:null,usdCny:1,face:0.03},{id:'gpt-5-official-old',rateCardId:'fixture-rate',model:'gpt-5',effectiveSecs:now-7200})));
const server=http.createServer(async(req,res)=>{
  try{
    if(req.url.startsWith('/api/'))return await fixture.handle(req,res);
    const file=path.resolve(root,decodeURIComponent(req.url.split('?')[0]).replace(/^\/admin\/?/,'')||'index.html');
    if(!file.startsWith(root+path.sep)||!fs.existsSync(file)){res.writeHead(404);return res.end();}
    res.setHeader('Content-Type',file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':'text/html');res.end(fs.readFileSync(file));
  }catch(error){res.writeHead(500);res.end(JSON.stringify({error:error.message}));}
});
const published=()=>fixture.writes.filter(write=>write.endpoint==='commercial-config');
const soon=version=>{const lead=version.effective_from_secs-Date.now()/1000;assert(lead>0&&lead<=125,`about a minute on (${lead})`);};
(async()=>{
  let browser;
  try{
    await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
    browser=await chromium.launch({headless:true,...(process.env.CHROME_PATH?{executablePath:process.env.CHROME_PATH}:{})});
    const page=await browser.newPage({viewport:{width:1440,height:1000}}),errors=[];
    page.setDefaultTimeout(10000);
    const origin=`http://127.0.0.1:${server.address().port}`;
    page.on('pageerror',error=>{errors.push(error.message);console.error('Browser error:',error.message);});
    const nativeDialogs=[];page.on('dialog',dialog=>{nativeDialogs.push(dialog.message());void dialog.dismiss();});
    await page.route('**/*',route=>new URL(route.request().url()).origin===origin?route.continue():route.abort());
    const button=name=>page.getByRole('button',{name,exact:true});
    const preview=page.getByRole('alertdialog');
    const toast=text=>page.locator('.toast').filter({hasText:text}).waitFor();
    const publishPreview=async({typed,ticks=0}={})=>{
      await preview.waitFor();const text=await preview.innerText();
      const checks=preview.getByRole('group',{name:'需要确认'}).getByRole('checkbox');
      assert.equal(await checks.count(),ticks,`${ticks} ticks asked for\n${text}`);
      for(let i=0;i<ticks;i++)await checks.nth(i).check();
      if(typed!==undefined){assert(await preview.locator('[data-confirm="accept"]').isDisabled(),'a flagged change needs its confirmation first');await preview.getByLabel('确认输入').fill(typed);}
      await preview.locator('[data-confirm="accept"]').click();await preview.waitFor({state:'detached'});return text;
    };
    const row=name=>page.getByRole('row').filter({hasText:name}).filter({has:page.getByRole('button',{name:'调价'})});
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();
    await button('刷新').waitFor();
    await page.getByRole('navigation').getByRole('button',{name:'模型与定价',exact:true}).click();

    // 调价 from the official price the price in force records; a price below the route's cost is
    // marked, names the model and asks for each loss-making route to be ticked.
    await row('claude-sonnet').getByRole('button',{name:'调价',exact:true}).click();
    const drawer=page.locator('#price-drawer');await drawer.waitFor();
    assert.equal(await drawer.getByLabel('官方输入价',{exact:true}).inputValue(),'3');
    assert.equal(await drawer.getByLabel('计费倍率',{exact:true}).inputValue(),'0.24');
    await drawer.getByText('来自现行价格记录的官方价').waitFor();
    await drawer.getByText('官方价 × ×0.08（供应商）').waitFor();
    await drawer.getByLabel('计费倍率',{exact:true}).fill('0.05');
    const results=drawer.getByRole('table',{name:'按官方价算出的价格'});
    assert((await results.innerText()).includes('24 → 5'),await results.innerText());
    await drawer.getByRole('status').filter({hasText:'毛利约 −'}).waitFor();
    await drawer.getByRole('status').filter({hasText:'示例 2,025 输入 + 480 输出'}).waitFor();
    await drawer.getByLabel('调价原因',{exact:true}).fill('限时低价');
    await drawer.getByRole('button',{name:'预览调价',exact:true}).click();
    const lossText=await publishPreview({typed:'claude-sonnet',ticks:1});
    for(const expected of ['官方价 $3 / $15 / $3.75 / $0.3（来自现行价格） × 计费倍率 ×0.05（原 ×0.24）','有线路亏损','变化 79%','会覆盖这次修改','claude-sonnet 经 测试供应商 / Fixture / claude-sonnet 的成本不低于售价','尽快，'])
      assert(lossText.includes(expected),`${expected}\n${lossText}`);
    await toast('已发布 claude-sonnet 的新价格');
    const cut=published().at(-1).body;
    assert.deepEqual(Object.keys(cut).sort(),['expected_revision','reason','versions']);
    const [low]=cut.versions;
    for(const [field,value] of Object.entries({model:'claude-sonnet',rate_card_id:'fixture-rate',fixed_input_credit_per_m:5000000,fixed_output_credit_per_m:25000000,input_price_per_m:0.24,margin_multiplier:1,currency:'CNY'}))assert.equal(low[field],value,field);
    assert.deepEqual([low.official.price_multiplier,low.official.cost_multiplier,low.official.credit_face_value_cny],[0.05,0.08,0.03]);soon(low);
    console.log('PASS: 调价 from the official price in force: 计费倍率 0.05 below the route cost is marked, the model typed and the route ticked; one version about a minute on');

    // The price just published and the promotion are both scheduled; the promotion is withdrawn.
    await drawer.waitFor({state:'detached'});
    await row('claude-sonnet').getByRole('button',{name:'调价',exact:true}).click();await drawer.waitFor();
    const promoLine=drawer.locator('.scheduled-price').filter({hasText:promo});
    await promoLine.getByRole('button',{name:'取消排期',exact:true}).click();
    await preview.getByLabel(/原因/).fill('促销取消');await preview.locator('[data-confirm="accept"]').click();
    await toast('的排期价格');
    assert.deepEqual(published().at(-1).body.cancelled_versions,[promo]);
    assert(!fixture.config.versions.some(version=>version.id===promo),'withdrawn');
    await promoLine.waitFor({state:'detached'});
    assert.equal(await drawer.locator('.scheduled-price').filter({hasText:low.id}).count(),1,'the new price stays scheduled');
    await drawer.getByRole('button',{name:'关闭调价',exact:true}).click();await drawer.waitFor({state:'detached'});
    console.log('PASS: a scheduled price withdrawn from the 调价 drawer with a reason (cancelled_versions), the other one kept');

    // 恢复上一版价格: the previous inputs, as a new version.
    await row('gpt-5').getByRole('button',{name:'调价',exact:true}).click();await drawer.waitFor();
    await drawer.getByRole('button',{name:'恢复上一版价格',exact:true}).click();
    assert.equal(await drawer.getByLabel('计费倍率',{exact:true}).inputValue(),'0.3');
    assert.match(await drawer.getByLabel('调价原因',{exact:true}).inputValue(),/^恢复上一版价格/);
    await drawer.getByRole('button',{name:'预览调价',exact:true}).click();await publishPreview();
    await toast('已发布 gpt-5 的新价格');
    const restored=published().at(-1).body.versions[0];
    assert.deepEqual([restored.model,restored.official.price_multiplier,restored.fixed_input_credit_per_m],['gpt-5',0.3,40000000]);soon(restored);
    console.log('PASS: 恢复上一版价格 publishes the previous version\'s inputs as a new version');

    // 批量调价: one 计费倍率 for three models; gemini-pro, typed the old way, is priced from the official price table.
    for(const name of ['gpt-5','gemini-pro','gpt-6-astra'])await page.getByLabel(`选择 ${name}`,{exact:true}).check();
    await page.getByRole('region',{name:'批量模型操作'}).getByRole('button',{name:'批量调价',exact:true}).click();
    const bulk=page.locator('#bulk-price');await bulk.waitFor();
    assert.equal(await bulk.getByLabel('批量计费倍率',{exact:true}).inputValue(),'0.24','the default 计费倍率 to start from');
    await bulk.getByLabel('批量计费倍率',{exact:true}).fill('0.3');
    const bulkTable=await bulk.getByRole('table',{name:'批量调价预览'}).innerText();
    assert(bulkTable.includes('旧版价格：改按官方价表')&&bulkTable.includes('3 / 15 → 20 / 120'),bulkTable);
    await bulk.getByLabel('批量调价原因',{exact:true}).fill('计费倍率统一 0.3');
    await bulk.getByRole('button',{name:'预览调价',exact:true}).click();
    const bulkText=await publishPreview({typed:'gemini-pro'});
    for(const expected of ['计费倍率都设为 ×0.3','这些模型原是旧版价格（直接填的积分），改为按官方价表定价：gemini-pro','变化 700%'])
      assert(bulkText.includes(expected),`${expected}\n${bulkText}`);
    await toast('已发布 3 个模型的新价格');
    const bulkBody=published().at(-1).body;
    assert.deepEqual(bulkBody.versions.map(version=>[version.model,version.official.price_multiplier,version.fixed_input_credit_per_m]),[['gpt-5',0.3,40000000],['gemini-pro',0.3,20000000],['gpt-6-astra',0.3,100000000]]);
    assert.equal(new Set(bulkBody.versions.map(version=>version.effective_from_secs)).size,1,'all from one time');
    assert.deepEqual(bulkBody.versions[1].official.input_usd_per_m,2,'from the official price table');
    console.log('PASS: 批量调价 sets one 计费倍率 for three models in one publication; the legacy one priced from the official price table and marked');

    // 上架 into two groups at once, hidden first, priced by another model's official price.
    await button('＋ 上架模型').click();
    const listing=page.locator('#listing-drawer');await listing.waitFor();
    await listing.getByLabel('供应商',{exact:true}).selectOption('fixture-openai');
    await listing.getByLabel('上游模型',{exact:true}).fill('gpt-5.6-sol');
    await listing.getByText('没有官方价：填四项官方价，或按其他模型定价').waitFor();
    await listing.getByLabel('按其他模型定价',{exact:true}).selectOption('gpt-6-astra');
    assert.equal(await listing.getByLabel('官方输出价',{exact:true}).inputValue(),'50');
    assert.equal(await listing.getByLabel('计费倍率',{exact:true}).inputValue(),'0.24');
    assert(await listing.getByRole('checkbox',{name:'PRO',exact:true}).isChecked(),'the first group that issues cards');
    await listing.getByLabel('PRO 的位置',{exact:true}).selectOption({label:'排在 claude-sonnet 之后'});
    await listing.getByRole('checkbox',{name:'PRO Max',exact:true}).check();
    await listing.getByLabel('PRO Max 的位置',{exact:true}).selectOption({label:'排在最前（成为默认模型）'});
    await listing.getByRole('checkbox',{name:/上架后先隐藏/}).check();
    await listing.getByLabel('最大输出',{exact:true}).fill('16K');await listing.getByRole('checkbox',{name:/推理/}).check();
    await listing.getByText('打开了推理，最大输出却不到 32K').first().waitFor();
    await listing.getByLabel('最大输出',{exact:true}).fill('128K');
    await listing.getByRole('button',{name:'预览上架',exact:true}).click();
    const listText=await publishPreview();
    for(const expected of ['客户看到：GPT 5.6 Sol（gpt-5.6-sol） · 先隐藏','PRO：排在 claude-sonnet 之后','PRO Max：排在最前（成为默认模型）','售价：官方价 $10 / $50 / $12.5 / $1（按 gpt-6-astra 定价） × 计费倍率 ×0.24 · 上架即生效','PRO、PRO Max'])
      assert(listText.includes(expected),`${expected}\n${listText}`);
    await toast('已上架 gpt-5.6-sol（先隐藏）');
    const listed=published().at(-1).body;
    assert.equal(listed.reason,'上架 gpt-5.6-sol（OpenAI 格式 / Fixture）（按 gpt-6-astra 定价）');
    assert.deepEqual(listed.models.map(model=>[model.id,model.group_id,model.sort_order,model.visible]),
      [['fixture-model-3','fixture-group-0',2,true],['fixture-openai-gpt-5.6-sol','fixture-group-0',1,false],['fixture-openai-gpt-5.6-sol-2','fixture-group-2',-1,false]]);
    assert.equal(listed.versions.length,1,'PRO and PRO Max share one price table');
    const [first]=listed.versions;
    for(const [field,value] of Object.entries({model:'gpt-5.6-sol',effective_from_secs:0,fixed_input_credit_per_m:80000000,input_price_per_m:0.6}))assert.equal(first[field],value,field);
    assert.deepEqual([first.official.input_usd_per_m,first.official.price_multiplier,first.official.cost_multiplier],[10,0.24,0.06]);
    assert(fixture.config.models.filter(model=>model.exposed_model_id==='gpt-5.6-sol').every(model=>model.visible===false),'listed hidden');
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log('PASS: 上架 into PRO and PRO Max at once, hidden first, each at its place, priced by gpt-6-astra\'s official price in one publication; the thinking-model warning');
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
