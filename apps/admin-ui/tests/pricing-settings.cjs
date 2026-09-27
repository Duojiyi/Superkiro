// 定价设置 and 官方价表: final build + loopback fixture (seeded with official prices), never production.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
const http=require('node:http'),fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict');
const fixture=require('./fixture-api.cjs')();
const {promo}=require('./pricing-seed.cjs')(fixture);
const root=path.resolve(__dirname,'../dist');
const server=http.createServer(async(req,res)=>{
  try{
    if(req.url.startsWith('/api/'))return await fixture.handle(req,res);
    const file=path.resolve(root,decodeURIComponent(req.url.split('?')[0]).replace(/^\/admin\/?/,'')||'index.html');
    if(!file.startsWith(root+path.sep)||!fs.existsSync(file)){res.writeHead(404);return res.end();}
    res.setHeader('Content-Type',file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':'text/html');res.end(fs.readFileSync(file));
  }catch(error){res.writeHead(500);res.end(JSON.stringify({error:error.message}));}
});
const published=()=>fixture.writes.filter(write=>write.endpoint==='commercial-config');
(async()=>{
  let browser;
  try{
    await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
    browser=await chromium.launch({headless:true,...(process.env.CHROME_PATH?{executablePath:process.env.CHROME_PATH}:{})});
    const page=await browser.newPage({viewport:{width:1440,height:1000},acceptDownloads:true}),errors=[];
    page.setDefaultTimeout(10000);
    const origin=`http://127.0.0.1:${server.address().port}`;
    page.on('pageerror',error=>{errors.push(error.message);console.error('Browser error:',error.message);});
    const nativeDialogs=[];page.on('dialog',dialog=>{nativeDialogs.push(dialog.message());void dialog.dismiss();});
    await page.route('**/*',route=>new URL(route.request().url()).origin===origin?route.continue():route.abort());
    const button=name=>page.getByRole('button',{name,exact:true});
    const preview=page.getByRole('alertdialog');
    const publishPreview=async({typed,ticks=0,option}={})=>{
      await preview.waitFor();const text=await preview.innerText();
      if(option!==undefined)await preview.getByRole('checkbox',{name:/^同时/}).setChecked(option);
      const checks=preview.getByRole('group',{name:'需要确认'}).getByRole('checkbox');
      assert.equal(await checks.count(),ticks,`${ticks} ticks asked for\n${text}`);
      for(let i=0;i<ticks;i++)await checks.nth(i).check();
      if(typed!==undefined){assert(await preview.locator('[data-confirm="accept"]').isDisabled(),'a flagged change needs its confirmation first');await preview.getByLabel('确认输入').fill(typed);}
      await preview.locator('[data-confirm="accept"]').click();await preview.waitFor({state:'detached'});return text;
    };
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();
    await button('刷新').waitFor();
    await page.getByRole('navigation').getByRole('button',{name:'模型与定价',exact:true}).click();
    await page.getByRole('tab',{name:'定价设置'}).click();
    const settings=page.getByRole('region',{name:'定价设置'});
    const reason=settings.getByLabel('定价设置变更原因',{exact:true}),open=settings.getByRole('button',{name:'预览并发布',exact:true});
    assert.equal(await settings.getByLabel('积分面值',{exact:true}).inputValue(),'0.03');
    await settings.getByText('1000 积分 = ¥30').waitFor();
    assert.equal(await settings.getByLabel('官方价 1 美元折合',{exact:true}).inputValue(),'1');

    // A new face value reprices every official price in one publication: in force from now, the
    // scheduled promotion withdrawn and added again at its own time; a typed legacy price keeps its credits.
    const promoAt=fixture.config.versions.find(version=>version.id===promo).effective_from_secs;
    await settings.getByLabel('积分面值',{exact:true}).fill('0.05');await settings.getByText('1000 积分 = ¥50').waitFor();
    await reason.fill('积分面值调到 0.05');await open.click();
    await preview.waitFor();
    const rows=await preview.getByRole('table',{name:'受影响的模型'}).locator('tbody tr').allInnerTexts();
    assert.equal(rows.length,4,rows.join('\n'));
    assert(rows[0].includes('claude-sonnet')&&rows[0].includes('24 → 14.4')&&rows[0].includes('−40%')&&rows[0].includes('¥0.72 / ¥3.6'),rows[0]);
    assert(!rows[0].includes('会覆盖这次修改'),'the promotion is repriced in the same publication');
    assert(rows[3].includes('gemini-pro')&&rows[3].includes('¥0.09 / ¥0.45 → ¥0.15 / ¥0.75')&&rows[3].includes('有线路亏损'),rows[3]);
    const faceText=await preview.innerText();
    for(const expected of ['积分面值 0.03 → 0.05 元/积分（1000 积分 = ¥50）','未消耗余额 5,400 积分：折合 ¥162 → ¥270','PRO+','¥0.0275','便宜 45.0%','这些模型没有官方价，积分不变','一起重算 4 个官方价版本，其中 1 个已排期的撤回后在原时间重新排期'])
      assert(faceText.includes(expected),`${expected}\n${faceText}`);
    await publishPreview({typed:'gemini-pro',ticks:1});
    await page.locator('.toast').filter({hasText:'已发布定价设置'}).waitFor();
    const face=published().at(-1).body;
    assert.deepEqual(Object.keys(face).sort(),['cancelled_versions','expected_revision','reason','settings','versions']);
    assert.deepEqual(face.settings,{credit_face_value_cny:0.05,usd_cny_rate:7.25},'only what changed, and the two the server always needs');
    assert.deepEqual(face.cancelled_versions,[promo]);
    assert.deepEqual(face.versions.map(v=>[v.model,v.effective_from_secs,v.fixed_output_credit_per_m,v.official.credit_face_value_cny,v.official.price_multiplier]),
      [['claude-sonnet',0,72000000,0.05,0.24],['gpt-5',0,96000000,0.05,0.24],['gpt-6-astra',0,240000000,0.05,0.24],['claude-sonnet',promoAt,60000000,0.05,0.2]]);
    assert.equal(fixture.config.settings.credit_face_value_cny,0.05);
    assert(fixture.config.versions.some(v=>v.model==='claude-sonnet'&&v.effective_from_secs===promoAt&&v.official.credit_face_value_cny===0.05),'the promotion keeps its time');
    assert.equal(fixture.config.versions.find(v=>v.id==='fixture-price-2').fixed_input_credit_per_m,3000000,'the legacy price keeps its credits');
    console.log('PASS: face value 0.03 → 0.05: every official price repriced in one publication (in force from now, the scheduled one withdrawn and re-added at its time), balances and plans at the new face value, the legacy model named and its loss confirmed');

    // A provider's 成本倍率 changes costs only; keeping each margin is offered separately.
    const openai=settings.getByLabel('OpenAI 格式 / Fixture 成本倍率',{exact:true});
    assert.equal(await openai.inputValue(),'0.06');assert.equal(await settings.getByLabel('停用的供应商 / Fixture 成本倍率',{exact:true}).getAttribute('placeholder'),'默认 0.08');
    await openai.fill('0.08');await settings.getByLabel('默认计费倍率',{exact:true}).fill('0.3');
    await reason.fill('Astra 涨价');await open.click();
    const costText=await publishPreview({option:false});
    for(const expected of ['默认计费倍率 ×0.24 → ×0.3（之后新定的价格用它）','供应商成本倍率：fixture-openai ×0.06 → ×0.08','gpt-6-astra','75% → 67%','同时把计费倍率调到保持毛利（gpt-6-astra ×0.24 → ×0.32'])
      assert(costText.includes(expected),`${expected}\n${costText}`);
    await page.locator('.toast').filter({hasText:'已发布定价设置'}).waitFor();
    assert.deepEqual(published().at(-1).body,{settings:{credit_face_value_cny:0.05,usd_cny_rate:7.25,default_price_multiplier:0.3,provider_cost_multipliers:{'fixture-provider':0.08,'fixture-openai':0.08}},
      expected_revision:published().at(-1).body.expected_revision,reason:'Astra 涨价'},'settings only: customer prices unchanged');
    await openai.fill('0.1');await reason.fill('Astra 再涨价，保持毛利');await open.click();
    await publishPreview({option:true});
    await page.locator('.toast').filter({hasText:'已发布定价设置'}).waitFor();
    const kept=published().at(-1).body;
    assert.deepEqual(kept.settings.provider_cost_multipliers,{'fixture-provider':0.08,'fixture-openai':0.1});
    assert.equal(kept.versions.length,1);const [astra]=kept.versions;
    for(const [field,value] of Object.entries({model:'gpt-6-astra',fixed_input_credit_per_m:60000000,input_price_per_m:1,margin_multiplier:1}))assert.equal(astra[field],value,field);
    assert.deepEqual([astra.official.price_multiplier,astra.official.cost_multiplier,astra.official.credit_face_value_cny],[0.3,0.1,0.05]);
    const lead=astra.effective_from_secs-Date.now()/1000;assert(lead>0&&lead<=125,`about a minute on (${lead})`);
    console.log('PASS: provider 成本倍率 and default 计费倍率: previewed margins, a settings-only publication; keeping the margin offered separately, as a new version about a minute on');

    // 官方价表: edit a row, paste CSV (a bad line reported), preview the route costs it moves, re-derive the price set from it.
    await page.getByRole('tab',{name:/官方价表/}).click();
    const table=page.getByRole('region',{name:'官方价表'});
    const row=name=>table.getByRole('row').filter({has:page.getByText(name,{exact:true})});
    await row('claude-sonnet').getByRole('button',{name:'编辑',exact:true}).click();
    await table.getByLabel('claude-sonnet 官方输入价',{exact:true}).fill('3.3');await table.getByRole('button',{name:'完成',exact:true}).click();
    await row('claude-sonnet').getByText('已修改').waitFor();
    await table.getByText('粘贴 CSV（model,input,output,cache_write,cache_read）').click();
    await table.getByLabel('粘贴官方价 CSV',{exact:true}).fill('model,input,output,cache_write,cache_read\nclaude-opus-5\t5\t25\t6.25\t0.5\nbad line\n');
    await table.getByRole('button',{name:'套用到表格',exact:true}).click();
    await table.getByRole('status').filter({hasText:'读到 1 行：新增 1、更新 0；1 行没读懂：第 3 行（bad line）：需要四个 0–10,000 的价格'}).waitFor();
    await row('claude-opus-5').getByText('新增').waitFor();
    await table.getByLabel('官方价变更原因',{exact:true}).fill('官网 9/27 调价');
    await table.getByRole('button',{name:'预览并发布',exact:true}).click();
    const officialText=await publishPreview({option:true});
    for(const expected of ['claude-sonnet：$3 / $15 / $3.75 / $0.3 → $3.3 / $15 / $3.75 / $0.3','新增 claude-opus-5：$5 / $25 / $6.25 / $0.5','同时按新官方价重算这些模型的售价（约 1 分钟后生效）：claude-sonnet','会覆盖这次修改'])
      assert(officialText.includes(expected),`${expected}\n${officialText}`);
    await page.locator('.toast').filter({hasText:'已发布官方价表'}).waitFor();
    const sentTable=published().at(-1).body;
    assert.deepEqual(Object.keys(sentTable.settings.official_prices).sort(),['claude-opus-5','claude-sonnet','gemini-pro','gpt-5','gpt-6-astra']);
    assert.deepEqual(sentTable.settings.official_prices['claude-opus-5'],{input_usd_per_m:5,output_usd_per_m:25,cache_creation_usd_per_m:6.25,cache_read_usd_per_m:0.5});
    assert.equal(sentTable.versions.length,1);
    assert.deepEqual([sentTable.versions[0].model,sentTable.versions[0].fixed_input_credit_per_m,sentTable.versions[0].official.input_usd_per_m],['claude-sonnet',15840000,3.3]);
    assert(fixture.config.settings.official_prices['claude-opus-5'].updated_at_secs>Date.now()/1000-60,'stamped by the server');
    const download=page.waitForEvent('download');await table.getByRole('button',{name:'导出 CSV',exact:true}).click();
    const csv=fs.readFileSync(await (await download).path(),'utf8');
    assert(csv.startsWith('model,input,output,cache_write,cache_read,note,updated_at\n')&&csv.includes('claude-opus-5,5,25,6.25,0.5,""')&&csv.includes('claude-sonnet,3.3,15,3.75,0.3,"官网 9/20"'),csv);
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log('PASS: 官方价表: edit and CSV paste (bad lines reported), preview of the costs it moves with re-deriving the price set from the changed row, one publication, exported as CSV');
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
