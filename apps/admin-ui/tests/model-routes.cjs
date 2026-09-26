// 线路 regression (backups, route costs, 切换线路, 切回): final build + loopback fixture, never production.
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
    // A backup: another provider, its own Key check, reorderable, and its own procurement price.
    await routes.getByRole('button',{name:'＋ 添加备用线路',exact:true}).click();
    const backup=n=>routes.getByRole('list',{name:'备用线路'}).locator('li').nth(n-1);
    assert.equal(await routes.getByLabel('备用线路 1 供应商',{exact:true}).inputValue(),'fixture-openai');
    await backup(1).getByText('OpenAI 格式 / Fixture 没有启用的 Key 授权 claude-sonnet').waitFor();
    await routes.getByLabel('备用线路 1 上游模型',{exact:true}).fill('gpt-6-astra');
    await backup(1).getByText('可用',{exact:true}).waitFor();
    await backup(1).getByRole('button',{name:'设置采购价',exact:true}).click();
    for(const [label,value] of [['输入','1'],['输出','5'],['缓存写','1.25'],['缓存读','0.1']])await routes.getByLabel(`备用线路 1 采购${label}价`,{exact:true}).fill(value);
    await backup(1).getByRole('button',{name:'暂存',exact:true}).click();
    await backup(1).getByText('线路采购价，随发布生效').waitFor();
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
    await button('发布').click();
    const facts=await confirm();assert(facts.includes('1 个新价格版本：1 个发布即生效'),facts);
    await page.locator('.toast').filter({hasText:'已发布'}).waitFor();
    const sent=published().at(-1).body;
    assert.deepEqual(sent.models.map(row=>[row.id,row.fallback_chain]),[['fixture-model-0',[{provider_id:'fixture-openai',target_model:'gpt-6-astra'}]]]);
    assert.equal(sent.versions.length,1);const [routeCost]=sent.versions;
    for(const [field,value] of Object.entries({model:'fixture-openai/gpt-6-astra',rate_card_id:'fixture-rate',effective_from_secs:0,currency:'CNY',output_price_per_m:5,fixed_input_credit_per_m:0,fixed_output_credit_per_m:0,per_call_credit:0}))
      assert.equal(routeCost[field],value,field);
    const row=name=>page.getByRole('row').filter({has:page.getByRole('button',{name:'编辑',exact:true})}).filter({hasText:name});
    await row('claude-sonnet').getByText('主 + 1 备',{exact:true}).waitFor();
    const costRow=page.getByRole('region',{name:'价格版本'}).getByRole('row').filter({hasText:'线路采购价'});
    assert((await costRow.innerText()).includes('OpenAI 格式 / Fixture / gpt-6-astra')&&(await costRow.innerText()).includes('不用于扣费'));
    // A second price for the same route cannot start now: it is sent for two minutes on.
    await page.getByRole('list',{name:'备用线路'}).locator('li').first().getByRole('button',{name:'设置采购价',exact:true}).click();
    assert.equal(await routes.getByLabel('备用线路 1 采购输出价',{exact:true}).inputValue(),'5','the form starts from the route price in force');
    await routes.getByLabel('备用线路 1 采购输出价',{exact:true}).fill('4');await page.getByRole('list',{name:'备用线路'}).getByRole('button',{name:'暂存',exact:true}).click();
    await bar.getByLabel('变更原因',{exact:true}).fill('备用线路降价');await button('发布').click();
    assert(/1 个新价格版本：1 个 \d\d:\d\d 起生效/.test(await confirm()));await page.locator('.toast').filter({hasText:'已发布'}).waitFor();
    const lead=published().at(-1).body.versions[0].effective_from_secs-Date.now()/1000;assert(lead>60&&lead<=180,`two minutes on (${lead})`);
    console.log('PASS: 线路: backups with their own Key check, reorder, remove, promote and back; a route cost staged and published at once, the next one later; 主 + N 备; labelled 线路采购价');

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
    await preview.getByText('新线路没有自己的采购价').waitFor();
    await preview.getByRole('button',{name:'设置新线路采购价',exact:true}).click();
    for(const [label,value] of [['输入','2'],['输出','8'],['缓存写','2.5'],['缓存读','0.2']])await drawer.getByLabel(`gpt-5 新线路采购${label}价`,{exact:true}).fill(value);
    await drawer.getByRole('button',{name:'切换',exact:true}).click();
    const switchFacts=await confirm();
    for(const expected of ['gpt-5：fixture-provider / gpt-5 → fixture-openai / gpt-5.6-sol','gemini-pro：fixture-provider / gemini-pro → fixture-openai / gpt-5.6-terra','同时设置 1 条新线路的采购价','保留原线路作为备用'])
      assert(switchFacts.includes(expected),`${expected}\n${switchFacts}`);
    await page.locator('.toast').filter({hasText:'已把 2 个模型切换到 OpenAI 格式 / Fixture'}).waitFor();
    const switched=published().at(-1).body;
    assert.deepEqual(switched.models.map(row=>[row.id,row.target_provider_id,row.target_model,row.fallback_chain]).sort(),[
      ['fixture-model-1','fixture-openai','gpt-5.6-sol',[{provider_id:'fixture-provider',target_model:'gpt-5'}]],
      ['fixture-model-2','fixture-openai','gpt-5.6-terra',[{provider_id:'fixture-provider',target_model:'gemini-pro'}]]]);
    assert.deepEqual(switched.versions.map(version=>[version.model,version.effective_from_secs,version.fixed_input_credit_per_m]),[['fixture-openai/gpt-5.6-sol',0,0]]);
    assert.equal(model('fixture-model-1').target_provider_id,'fixture-openai');
    // 切回: one step back to each model's route before the switch.
    const note=page.getByRole('status').filter({hasText:'已把 gpt-5、gemini-pro 切换到 OpenAI 格式 / Fixture，原线路保留为第 1 条备用'});await note.waitFor();
    await note.getByRole('button',{name:'切回原线路',exact:true}).click();
    assert((await confirm()).includes('gpt-5：fixture-openai → fixture-provider / gpt-5'));
    await page.locator('.toast').filter({hasText:'已切回 2 个模型的原线路'}).waitFor();
    assert.deepEqual(published().at(-1).body.models.map(row=>[row.id,row.target_provider_id,row.target_model,row.fallback_chain]).sort(),[
      ['fixture-model-1','fixture-provider','gpt-5',[]],['fixture-model-2','fixture-provider','gemini-pro',[]]]);
    assert.equal(await note.count(),0);
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log('PASS: 切换线路: Key checks refuse a stranded model first, upstream names editable, a new route cost on the way, old route kept as backup; 切回 restores each route');
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
