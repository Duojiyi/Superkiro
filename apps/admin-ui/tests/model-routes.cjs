// 线路 regression (backups, route costs, 切换线路, 切回): final build + loopback fixture, never production.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
const recordToasts=require('./toasts.cjs');
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
    const toasts=await recordToasts(page);
    const origin=`http://127.0.0.1:${server.address().port}`;
    page.on('pageerror',error=>{errors.push(error.message);console.error('Browser error:',error.message);});
    const nativeDialogs=[];page.on('dialog',dialog=>{nativeDialogs.push(dialog.message());void dialog.dismiss();});
    await page.route('**/*',route=>new URL(route.request().url()).origin===origin?route.continue():route.abort());
    const button=name=>page.getByRole('button',{name,exact:true});
    const nav=name=>page.getByRole('navigation').getByRole('button',{name,exact:true}).click();
    const confirm=async({option}={})=>{
      const box=page.getByRole('alertdialog');await box.waitFor();const text=await box.innerText();
      if(option!==undefined)await box.getByRole('checkbox').setChecked(option);
      await box.locator('[data-confirm="accept"]').click();await box.waitFor({state:'detached'});return text;
    };
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();
    await button('刷新').waitFor();
    await nav('模型与定价');
    const routes=page.getByRole('region',{name:'线路'});await routes.waitFor();
    assert.equal(await page.locator('.route-primary').getByText('可用',{exact:true}).count(),1,'the primary route passes the Key check');
    // A backup: another provider (one that can serve the model, when there is one), its own Key check, reorderable, and its own cost.
    await routes.getByRole('button',{name:'＋ 添加备用线路',exact:true}).click();
    const backup=n=>routes.getByRole('list',{name:'备用线路'}).locator('li').nth(n-1);
    assert.equal(await routes.getByLabel('备用线路 1 供应商',{exact:true}).inputValue(),'fixture-openai','no other provider serves claude-sonnet: the first enabled one, marked');
    await backup(1).getByText('OpenAI 格式 / Fixture 没有启用的 Key 授权 claude-sonnet').waitFor();
    await routes.getByLabel('备用线路 1 上游模型',{exact:true}).fill('gpt-6-astra');
    await backup(1).getByText('可用',{exact:true}).waitFor();
    // A route's own cost publishes on its own, so not while the page has unpublished edits.
    const setCost=backup(1).getByRole('button',{name:'设置线路成本',exact:true});
    assert(await setCost.isDisabled());assert.equal(await setCost.getAttribute('title'),'先发布或放弃未发布的修改，再设置线路成本');
    await backup(1).getByText('旧版：按上游模型的价格版本').waitFor();
    await routes.getByRole('button',{name:'＋ 添加备用线路',exact:true}).click();
    await routes.getByLabel('备用线路 2 供应商',{exact:true}).selectOption('fixture-disabled');
    await backup(2).getByText('停用的供应商 / Fixture 已停用').waitFor();
    await routes.getByRole('button',{name:'备用线路 2 上移',exact:true}).click();
    assert.equal(await routes.getByLabel('备用线路 1 供应商',{exact:true}).inputValue(),'fixture-disabled');
    await routes.getByRole('button',{name:'备用线路 2 上移',exact:true}).click();
    await backup(2).getByRole('button',{name:'移除',exact:true}).click();
    assert.equal(await routes.getByRole('list',{name:'备用线路'}).locator('li').count(),1);
    // 设为主线路 swaps it with the primary; doing it again swaps back.
    await backup(1).getByRole('button',{name:'设为主线路',exact:true}).click();
    assert.equal(await page.locator('.mapping-editor').getByLabel('供应商',{exact:true}).inputValue(),'fixture-openai');
    assert.equal(await routes.getByLabel('备用线路 1 上游模型',{exact:true}).inputValue(),'claude-sonnet');
    await backup(1).getByRole('button',{name:'设为主线路',exact:true}).click();
    assert.equal(await page.locator('.mapping-editor').getByLabel('上游模型',{exact:true}).inputValue(),'claude-sonnet');
    const bar=page.getByRole('region',{name:'发布'});
    await bar.getByLabel('变更原因',{exact:true}).fill('加一条备用线路');
    toasts.mark();await button('发布').click();
    await confirm();
    await toasts.shown('已发布');
    const sent=published().at(-1).body;
    assert.deepEqual(sent.models.map(row=>[row.id,row.fallback_chain]),[['fixture-model-0',[{provider_id:'fixture-openai',target_model:'gpt-6-astra'}]]]);
    assert.equal('versions' in sent,false,'no route cost version is made');
    const row=name=>page.getByRole('row').filter({has:page.getByRole('button',{name:'编辑',exact:true})}).filter({hasText:name});
    await row('claude-sonnet').getByText('主 + 1 备',{exact:true}).waitFor();
    // 设置线路成本: what this route bills (its own multiplier and basis), kept in the pricing settings for every group.
    await page.getByRole('list',{name:'备用线路'}).locator('li').first().getByRole('button',{name:'设置线路成本',exact:true}).click();
    const costDrawer=page.locator('#route-cost');await costDrawer.waitFor();
    await costDrawer.getByLabel('这条线路的成本倍率',{exact:true}).fill('0.1');
    for(const [label,value] of [['输入','0.1'],['输出','0.5'],['缓存写','0.1'],['缓存读','0.01']])await costDrawer.getByLabel(`计费基准${label}价`,{exact:true}).fill(value);
    await costDrawer.getByRole('status').filter({hasText:'输入 ¥0.01 · 输出 ¥0.05'}).waitFor();
    await costDrawer.getByLabel('线路成本原因',{exact:true}).fill('Astra 按这个价格收费');
    await costDrawer.getByRole('button',{name:'预览并发布',exact:true}).click();
    const costFacts=await confirm();
    // Every model routed through it is in the preview: claude-sonnet's backup and gpt-6-astra's primary.
    assert(costFacts.includes('OpenAI 格式 / Fixture / gpt-6-astra：成本倍率 — → ×0.1')&&costFacts.includes('上游计费基准 官方价 → $0.1 / $0.5 / $0.1 / $0.01')&&costFacts.includes('备1 OpenAI 格式 / Fixture / gpt-6-astra')&&costFacts.includes('主 OpenAI 格式 / Fixture / gpt-6-astra'),costFacts);
    await toasts.shown('已发布 OpenAI 格式 / Fixture / gpt-6-astra 的线路成本');await costDrawer.waitFor({state:'detached'});
    const costSent=published().at(-1).body;
    assert.deepEqual(Object.keys(costSent).sort(),['expected_revision','reason','settings']);
    assert.deepEqual(costSent.settings,{credit_face_value_cny:0.01,usd_cny_rate:7.2,route_costs:{'fixture-openai/gpt-6-astra':{cost_multiplier:0.1,basis_usd_per_m:[0.1,0.5,0.1,0.01]}}});
    await page.getByRole('list',{name:'备用线路'}).locator('li').first().getByText('成本 ¥0.01 / ¥0.05 每百万（入 / 出） · 线路计费基准 × ×0.1（这条线路）').waitFor();
    // Removing it goes back to the provider's multiplier and the official price (here: the price versions).
    await page.getByRole('list',{name:'备用线路'}).locator('li').first().getByRole('button',{name:'设置线路成本',exact:true}).click();await costDrawer.waitFor();
    assert.equal(await costDrawer.getByLabel('这条线路的成本倍率',{exact:true}).inputValue(),'0.1','the form starts from the route cost in force');
    await costDrawer.getByLabel('线路成本原因',{exact:true}).fill('不再单独计价');
    await costDrawer.getByRole('button',{name:'删除这条设置',exact:true}).click();
    // Back on the fixture's USD procurement prices both models lose money again: their count is typed.
    await page.getByRole('alertdialog').getByLabel('确认输入').fill('2');await confirm();
    await costDrawer.waitFor({state:'detached'});
    assert.deepEqual(published().at(-1).body.settings.route_costs,{});assert.deepEqual(fixture.config.settings.route_costs,{});
    console.log('PASS: 线路: backups with their own Key check, reorder, remove, promote and back; 主 + N 备; a route\'s own cost set, shown and removed through the pricing settings, never as a price version');

    // 切换线路: two models to the OpenAI-format provider; a model left without a usable route is refused first.
    for(const name of ['gpt-5','gemini-pro'])await page.getByRole('checkbox',{name:`选择 ${name}`,exact:true}).check();
    const selection=page.getByRole('region',{name:'批量模型操作'});await selection.getByText('已选 2 个模型').waitFor();
    await selection.getByRole('button',{name:'切换线路',exact:true}).click();
    const drawer=page.locator('#route-switch');await drawer.waitFor();
    assert.equal(await drawer.getByLabel('新供应商',{exact:true}).inputValue(),'fixture-openai');
    await drawer.getByText('OpenAI 格式 / Fixture 没有启用的 Key 授权 gpt-5').waitFor();
    await drawer.getByLabel('切换原因',{exact:true}).fill('主供应商维护');
    const before=published().length;
    await drawer.getByRole('button',{name:'切换',exact:true}).click();
    await drawer.getByRole('alert').filter({hasText:'这些在售模型的新线路不能用：gpt-5（OpenAI 格式 / Fixture 没有启用的 Key 授权 gpt-5）'}).waitFor();
    assert.equal(published().length,before);
    await drawer.getByLabel('gpt-5 切换后上游模型',{exact:true}).fill('gpt-5.6-sol');await drawer.getByLabel('gemini-pro 切换后上游模型',{exact:true}).fill('gpt-5.6-terra');
    const preview=drawer.getByRole('row').filter({has:page.getByLabel('gpt-5 切换后上游模型',{exact:true})});
    await preview.getByText('可用',{exact:true}).waitFor();
    await preview.getByText('新线路成本未知：它的上游模型没有官方价').waitFor();
    await drawer.getByRole('button',{name:'切换',exact:true}).click();
    const switchFacts=await confirm();
    for(const expected of ['gpt-5：fixture-provider / gpt-5 → fixture-openai / gpt-5.6-sol','gemini-pro：fixture-provider / gemini-pro → fixture-openai / gpt-5.6-terra','新线路的成本按旧版采购价估算或未知：gpt-5、gemini-pro','保留原线路作为备用'])
      assert(switchFacts.includes(expected),`${expected}\n${switchFacts}`);
    await toasts.shown('已把 2 个模型切换到 OpenAI 格式 / Fixture');
    const switched=published().at(-1).body;
    assert.deepEqual(switched.models.map(row=>[row.id,row.target_provider_id,row.target_model,row.fallback_chain]).sort(),[
      ['fixture-model-1','fixture-openai','gpt-5.6-sol',[{provider_id:'fixture-provider',target_model:'gpt-5'}]],
      ['fixture-model-2','fixture-openai','gpt-5.6-terra',[{provider_id:'fixture-provider',target_model:'gemini-pro'}]]]);
    assert.equal('versions' in switched,false,'switching makes no route cost version');
    assert.equal(model('fixture-model-1').target_provider_id,'fixture-openai');
    // 切回: one step back to each model's route before the switch.
    const note=page.getByRole('status').filter({hasText:'已把 gpt-5、gemini-pro 切换到 OpenAI 格式 / Fixture，原线路保留为第 1 条备用'});await note.waitFor();
    await note.getByRole('button',{name:'切回原线路',exact:true}).click();
    assert((await confirm()).includes('gpt-5：fixture-openai → fixture-provider / gpt-5'));
    await toasts.shown('已切回 2 个模型的原线路');
    assert.deepEqual(published().at(-1).body.models.map(row=>[row.id,row.target_provider_id,row.target_model,row.fallback_chain]).sort(),[
      ['fixture-model-1','fixture-provider','gpt-5',[]],['fixture-model-2','fixture-provider','gemini-pro',[]]]);
    assert.equal(await note.count(),0);
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log('PASS: 切换线路: Key checks refuse a stranded model first, upstream names editable, each route cost shown, old route kept as backup; 切回 restores each route');
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
