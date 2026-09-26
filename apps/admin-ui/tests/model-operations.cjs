// 模型与定价 operations (list order and default): final build + loopback fixture, never production.
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
const published=()=>fixture.writes.filter(write=>write.endpoint==='commercial-config');
const model=id=>fixture.config.models.find(row=>row.id===id);
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
    const nav=name=>page.getByRole('navigation').getByRole('button',{name,exact:true}).click();
    const confirm=async({option,reason,typed}={})=>{
      const box=page.getByRole('alertdialog');await box.waitFor();const text=await box.innerText();
      if(option!==undefined)await box.getByRole('checkbox').setChecked(option);
      if(reason!==undefined){assert(await box.locator('[data-confirm="accept"]').isDisabled(),'a reason is required first');await box.locator('#confirm-reason').fill(reason);}
      if(typed!==undefined)await box.getByLabel('确认输入').fill(typed);
      await box.locator('[data-confirm="accept"]').click();await box.waitFor({state:'detached'});return text;
    };
    const bar=page.getByRole('region',{name:'发布'});
    const publish=async reason=>{await bar.getByLabel('变更原因',{exact:true}).fill(reason);await button('发布').click();await confirm();await page.locator('.toast').filter({hasText:'已发布'}).waitFor();return published().at(-1).body;};
    const group=name=>page.getByRole('rowgroup',{name,exact:true});
    const names=name=>group(name).locator('td.cell-strong').evaluateAll(cells=>cells.map(cell=>cell.firstChild.textContent));
    // gpt-6-astra shares claude-sonnet's place in PRO: which one is Kiro's default is the server's order.
    model('fixture-model-3').sort_order=0;
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();
    await button('刷新').waitFor();
    await nav('模型与定价');
    const header=group('PRO').locator('tr.group-row');
    await header.getByText('Kiro 默认：claude-sonnet 或 gpt-6-astra（谁在前以服务器为准）').waitFor();
    await header.getByText('claude-sonnet、gpt-6-astra 排在同一位置').waitFor();
    assert.equal(await group('PRO').getByText('默认待定',{exact:true}).count(),2);
    assert.deepEqual(await names('PRO'),['claude-sonnet','gpt-6-astra']);
    assert.deepEqual(await names('PRO+'),['gpt-5'],'each group lists its own models');
    // 设为默认: to the top, the group numbered again; only the entries whose place changed are sent.
    await button('gpt-6-astra 的更多操作').click();await page.getByRole('menuitem',{name:'设为默认（排到最前）',exact:true}).click();
    assert.deepEqual(await names('PRO'),['gpt-6-astra','claude-sonnet']);
    await header.getByText('Kiro 默认：gpt-6-astra').waitFor();
    assert.equal(await header.getByText('排在同一位置').count(),0,'no tie is left');
    let sent=await publish('gpt-6-astra 设为默认');
    assert.deepEqual(sent.models.map(row=>[row.id,row.sort_order]),[['fixture-model-0',1]]);
    assert.equal(model('fixture-model-0').sort_order,1);
    await group('PRO').getByRole('row').filter({hasText:'gpt-6-astra'}).getByText('默认',{exact:true}).waitFor();
    // ▲▼ move one place and renumber.
    assert(await button('上移 gpt-6-astra').isDisabled());assert(await button('下移 claude-sonnet').isDisabled());
    await button('上移 claude-sonnet').click();
    assert.deepEqual(await names('PRO'),['claude-sonnet','gpt-6-astra']);
    sent=await publish('claude-sonnet 排回第一');
    assert.deepEqual(sent.models.map(row=>[row.id,row.sort_order]).sort(),[['fixture-model-0',0],['fixture-model-3',1]]);
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log("PASS: list order: each group in Kiro's order, a tie at the top named and its default marked undecided, 设为默认 and ▲▼ renumber and publish only the moved entries");

    // 批量调价: −10% for two models, previewed, one publication with one version each, costs kept.
    for(const version of fixture.config.versions)if(version.currency===undefined)Object.assign(version,{currency:'USD',input_price_per_m:1,output_price_per_m:2,cache_creation_price_per_m:1,cache_read_price_per_m:0.1,margin_multiplier:1});
    fixture.config.revision='fixture-rev-40';await button('刷新').click();await page.locator('.btn-refresh:not([disabled])').waitFor();
    for(const name of ['claude-sonnet','gpt-6-astra'])await page.getByRole('checkbox',{name:`选择 ${name}`,exact:true}).check();
    await page.getByRole('region',{name:'批量模型操作'}).getByRole('button',{name:'批量调价',exact:true}).click();
    const bulk=page.locator('#bulk-price');await bulk.waitFor();
    await bulk.getByLabel('调价百分比',{exact:true}).fill('-10');
    const line=name=>bulk.getByRole('row').filter({hasText:name});
    assert.equal(await line('claude-sonnet').locator('td').nth(2).innerText(),'2.7 / 13.5');
    assert.equal(await line('claude-sonnet').locator('td').nth(3).innerText(),'−10% / −10%');
    assert.equal(await line('gpt-6-astra').locator('td').nth(2).innerText(),'1.125 / 9');
    assert(/^-?\d+% → -?\d+%$/.test(await line('gpt-6-astra').locator('td').nth(4).innerText()),'the sample margin before and after');
    await bulk.getByLabel('批量调价原因',{exact:true}).fill('统一降价 10%');
    await bulk.getByRole('button',{name:'发布调价',exact:true}).click();
    const bulkFacts=await confirm();
    assert(bulkFacts.includes('claude-sonnet：输入 3 → 2.7，输出 15 → 13.5 积分/百万')&&bulkFacts.includes('gpt-6-astra：输入 1.25 → 1.125，输出 10 → 9 积分/百万'),bulkFacts);
    await page.locator('.toast').filter({hasText:'已发布 2 个模型的新价格'}).waitFor();await bulk.waitFor({state:'detached'});
    const priced=published().at(-1).body;
    assert.deepEqual(Object.keys(priced).sort(),['expected_revision','reason','versions']);
    assert.deepEqual(priced.versions.map(version=>[version.model,version.fixed_input_credit_per_m,version.fixed_output_credit_per_m,version.fixed_cache_read_credit_per_m,version.output_price_per_m,version.currency]).sort(),
      [['claude-sonnet',2700000,13500000,270000,2,'USD'],['gpt-6-astra',1125000,9000000,112500,10,'USD']]);
    const lead=priced.versions[0].effective_from_secs-Date.now()/1000;assert(lead>240&&lead<=360,`one time for all, five minutes on (${lead})`);
    assert.equal(priced.versions[0].effective_from_secs,priced.versions[1].effective_from_secs);
    assert.equal(await page.getByRole('region',{name:'批量模型操作'}).count(),0,'the selection is cleared');
    // 官方价 × 售价倍率 for gpt-5: the official prices are typed in the preview and remembered.
    await page.getByRole('checkbox',{name:'选择 gpt-5',exact:true}).check();
    await page.getByRole('region',{name:'批量模型操作'}).getByRole('button',{name:'批量调价',exact:true}).click();await bulk.waitFor();
    await bulk.getByRole('radio',{name:'官方价 × 售价倍率',exact:true}).click();
    await bulk.getByLabel('售价倍率',{exact:true}).fill('0.24');
    await line('gpt-5').getByText('填写四项官方价').waitFor();
    for(const [label,value] of [['输入','4'],['输出','20'],['缓存写','5'],['缓存读','0.4']])await bulk.getByLabel(`gpt-5 官方${label}价`,{exact:true}).fill(value);
    assert.equal(await line('gpt-5').locator('td').nth(3).innerText(),'96 / 480');
    await bulk.getByLabel('批量调价原因',{exact:true}).fill('按官方价');
    await bulk.getByRole('button',{name:'发布调价',exact:true}).click();await confirm();
    await page.locator('.toast').filter({hasText:'已发布 1 个模型的新价格'}).waitFor();
    assert.deepEqual(published().at(-1).body.versions.map(version=>[version.model,version.fixed_input_credit_per_m,version.fixed_cache_read_credit_per_m]),[['gpt-5',96000000,9600000]]);
    assert.deepEqual(await page.evaluate(()=>JSON.parse(localStorage.getItem('admin-official-prices:v1'))),{'gpt-5':['4','20','5','0.4']});
    console.log('PASS: 批量调价: ± percent previewed with margins and published as one version per model at one time; official price × retail multiplier, remembered');

    // 调价 has the same calculator: the remembered official prices and retail multiplier, this provider's cost multiplier.
    await page.getByRole('row').filter({hasText:'gpt-5'}).filter({has:page.getByRole('button',{name:'调价'})}).getByRole('button',{name:'调价',exact:true}).click();
    const drawer=page.locator('#price-drawer');await drawer.waitFor();
    await drawer.getByText('按官方价计算',{exact:true}).click();
    assert.equal(await drawer.getByLabel('官方输出价',{exact:true}).inputValue(),'20');assert.equal(await drawer.getByLabel('售价倍率',{exact:true}).inputValue(),'0.24');
    await drawer.getByLabel('售价倍率',{exact:true}).fill('0.3');await drawer.getByLabel('成本倍率',{exact:true}).fill('0.06');
    await drawer.getByRole('button',{name:'计算',exact:true}).click();
    assert.equal(await drawer.getByLabel('新输入售价',{exact:true}).inputValue(),'120');assert.equal(await drawer.getByLabel('新缓存读售价',{exact:true}).inputValue(),'12');
    assert.equal(await drawer.getByLabel('采购输出价',{exact:true}).inputValue(),'1.2');assert.equal(await drawer.getByLabel('采购价币种',{exact:true}).inputValue(),'CNY');
    assert.deepEqual(await page.evaluate(()=>JSON.parse(localStorage.getItem('admin-listing-rates:v2'))),{retail:'0.3',upstream:{'fixture-provider':'0.06'}});
    await drawer.getByRole('button',{name:'取消',exact:true}).click();await drawer.waitFor({state:'detached'});
    console.log('PASS: 调价 computes new prices and costs from official prices, remembering the multipliers per provider');

    // 隐藏 / 下架 / 恢复 / 删除, each confirmed with what it does, a reason, and the state shown.
    const menu=async(name,item)=>{await button(`${name} 的更多操作`).click();const entry=page.getByRole('menuitem',{name:item,exact:true});await entry.waitFor();return entry;};
    const state=name=>page.getByRole('row').filter({has:page.getByRole('button',{name:`${name} 的更多操作`,exact:true})}).locator('td.col-status');
    assert.equal(await state('claude-sonnet').innerText(),'在售');
    assert(await (await menu('claude-sonnet','删除（仅隐藏或已下架的条目）')).isDisabled(),'a model on sale cannot be deleted');
    await page.keyboard.press('Escape');
    await (await menu('claude-sonnet','隐藏（已在用的客户仍可用）')).click();
    const hideFacts=await confirm({reason:'先不对新客户开放'});
    assert(hideFacts.includes('它是 PRO 的默认模型：之后默认变为 gpt-6-astra')&&hideFacts.includes('已经在用这个模型 ID 的客户仍可继续调用'),hideFacts);
    await page.locator('.toast').filter({hasText:'已隐藏 claude-sonnet'}).waitFor();
    let sentState=published().at(-1).body;
    assert.deepEqual([sentState.reason,sentState.models.map(row=>[row.id,row.visible,row.retired])],['先不对新客户开放',[['fixture-model-0',false,false]]]);
    assert.equal(await state('claude-sonnet').innerText(),'隐藏');
    await header.getByText('Kiro 默认：gpt-6-astra').waitFor();
    await (await menu('claude-sonnet','下架（停止服务）')).click();
    assert((await confirm({reason:'上游停止供应'})).includes('所有请求都会被拒绝，包括已经在用的客户'));
    await page.locator('.toast').filter({hasText:'已下架 claude-sonnet'}).waitFor();
    assert.deepEqual(published().at(-1).body.models.map(row=>[row.id,row.visible,row.retired]),[['fixture-model-0',false,true]]);
    assert.equal(await state('claude-sonnet').innerText(),'已下架');
    // A server that does not keep 已下架 is called out, not shown as retired.
    let strip=true;
    await page.route('**/api/v1/admin/commercial-config',async route=>{
      if(route.request().method()!=='POST'||!strip)return route.fallback();
      strip=false;const response=await route.fetch(),body=await response.json();
      for(const row of body.config.models)delete row.retired;
      return route.fulfill({response,json:body});
    });
    await (await menu('gemini-pro','下架（停止服务）')).click();await confirm({reason:'试下架'});
    await page.getByRole('status').filter({hasText:'服务器没有记下“已下架”（可能还不支持）：gemini-pro 已隐藏，但已经在用它的客户仍能调用'}).waitFor();
    await button('刷新').click();await page.locator('.btn-refresh:not([disabled])').waitFor();
    // 恢复 needs a route that serves and a price in force.
    await (await menu('claude-sonnet','恢复')).click();await confirm({reason:'恢复供应'});
    await page.locator('.toast').filter({hasText:'已恢复 claude-sonnet'}).waitFor();
    assert.deepEqual(published().at(-1).body.models.map(row=>[row.id,row.visible,row.retired]),[['fixture-model-0',true,false]]);
    assert.equal(await state('claude-sonnet').innerText(),'在售');
    const key=fixture.keys.find(row=>row.id==='fixture-key'),allowed=key.allowed_models;key.allowed_models=allowed.filter(name=>name!=='gemini-pro');
    await button('刷新').click();await page.locator('.btn-refresh:not([disabled])').waitFor();
    const count=published().length;
    await (await menu('gemini-pro','恢复')).click();
    await page.getByRole('status').filter({hasText:'不能恢复 gemini-pro：主线路不能用（测试供应商 / Fixture 没有启用的 Key 授权 gemini-pro）'}).waitFor();
    assert.equal(published().length,count,'nothing is sent');
    key.allowed_models=allowed;
    // 删除: only hidden or retired entries, the word typed, and a reason.
    await (await menu('gemini-pro','删除（仅隐藏或已下架的条目）')).click();
    assert((await confirm({reason:'不再供应',typed:'删除'})).includes('不能撤销'));
    await page.locator('.toast').filter({hasText:'已删除 gemini-pro'}).waitFor();
    assert.deepEqual(Object.keys(published().at(-1).body).sort(),['expected_revision','reason','removed_models']);
    assert.deepEqual(published().at(-1).body.removed_models,['fixture-model-2']);
    assert.equal(await page.getByRole('rowgroup',{name:'PRO Max',exact:true}).count(),0,'the group without models is no longer listed');
    console.log('PASS: 隐藏 / 下架 / 恢复 / 删除: consequences and the next default stated, reasons required, state column, a server not keeping 已下架 called out, 恢复 refused without a route, 删除 only when not on sale');
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
