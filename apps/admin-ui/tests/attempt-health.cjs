// Health by attempt: 服务健康 and the provider cards count every upstream attempt (失败, 被备用接管, failures
// by kind), Key rows show their 24 hours, and 需要关注 names the models that fail with a link to their
// failed requests; an older server's traces are counted the same way. Final build + loopback fixture.
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
const now=Math.floor(Date.now()/1000);
// 瀚月 Max is claude-opus-5-5's primary. In the last hour it answered 529 to all twelve requests: seven
// failed outright, and for five the test provider took over and answered.
fixture.providers.push({id:'hanyue-max',name:'瀚月 Max',base_url:'https://max.hanyue.invalid',enabled:true,format:'anthropic'});
fixture.keys.push({id:'hanyue-max-key-1',provider_id:'hanyue-max',allowed_models:['claude-opus-5-5'],weight:1,enabled:true,health_state:'healthy'});
// claude-opus-5-5 is listed on 模型与定价, served by 瀚月 Max with the test provider as its backup.
fixture.config.models.push({id:'fixture-model-opus',exposed_model_id:'claude-opus-5-5',target_provider_id:'hanyue-max',target_model:'claude-opus-5-5',group_id:'fixture-group-0',context_window:200000,max_output:64000,
  credit_multiplier:1,visible:true,supports_tools:true,supports_vision:true,supports_reasoning:true,sort_order:0,aliases:[],fallback_chain:[{provider_id:'fixture-provider',target_model:'claude-sonnet'}],
  display_name:null,description:null,rate_multiplier:null,retired:false});
const hanyue={provider_id:'hanyue-max',key_id:'hanyue-max-key-1',success:false,error:'http_529',latency_ms:900};
const request=(id,ts,fields)=>({id,card_id:'fixture-card-0',ts,invocation_id:`fixture-card-0:${id}`,exposed_model:'claude-opus-5-5',status:'error',ttft_ms:null,tokens_per_second:null,
  error_class:'upstream_start_failed',provider_id:null,input_tokens:0,output_tokens:0,credits_charged:0,provider_cost_micro_cny:0,attempt_chain:[hanyue],...fields});
for(let i=0;i<7;i++)fixture.traces.push(request(`health-failed-${i}`,now-30-i*60));
for(let i=0;i<5;i++)fixture.traces.push(request(`health-takeover-${i}`,now-500-i*60,{status:'success',error_class:null,provider_id:'fixture-provider',credits_charged:1500000,input_tokens:1000,output_tokens:200,ttft_ms:900,
  attempt_chain:[hanyue,{provider_id:'fixture-provider',key_id:'fixture-key',success:true,error:null,latency_ms:700}]}));
// claude-opus-4-8: every request of the last day failed, none in the last hour.
for(let i=0;i<3;i++)fixture.traces.push(request(`health-old-${i}`,now-7200-i*3600,{exposed_model:'claude-opus-4-8',attempt_chain:[{provider_id:'fixture-provider',key_id:'fixture-key',success:false,error:'http_401',latency_ms:300}]}));
fixture.traces.sort((a,b)=>b.ts-a.ts);
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
    const attention=page.locator('.attention-list');
    const health=page.getByRole('region',{name:'服务健康'});
    const cells=async name=>health.getByRole('row').filter({hasText:name}).locator('td').allInnerTexts();
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();await button('刷新').waitFor();

    // 需要关注 names each failing model, and leads to its failed requests and, when it is listed, to the model.
    const failing=attention.locator('li').filter({has:page.getByText('claude-opus-5-5 近 1 小时 7 次失败（共 12 次）：上游未响应（未开始输出）',{exact:true})});await failing.waitFor();
    assert((await failing.locator('.attention-item').getAttribute('class')).includes('is-danger'));
    await attention.getByRole('button',{name:'claude-opus-4-8 近 24 小时 3 次请求全部失败：上游未响应（未开始输出）'}).waitFor();
    // 服务健康: every attempt, the failures (coloured), the takeovers and the failures by kind.
    const hanyueRow=await cells('瀚月 Max');
    // 拒绝 (the upstream refusing the request itself) is apart from 失败: none here.
    assert.deepEqual(hanyueRow.slice(3,8),['12','12（100.0%）','—','5','HTTP 529 · 上游过载 × 12'],hanyueRow.join(' | '));
    assert.equal(await health.getByRole('row').filter({hasText:'瀚月 Max'}).locator('td.num').nth(1).getAttribute('class'),'num is-danger');
    const fixtureRow=await cells('测试供应商 / Fixture');
    assert.deepEqual(fixtureRow.slice(3,7),['59','11（18.6%）','—','—'],'its own attempts and failures, the ones it took over counted as its successes');
    assert.equal(await health.locator('.estimate-tag').count(),0,'from the server: not an estimate');
    await failing.getByRole('button',{name:'查看失败请求',exact:true}).click();
    await page.getByRole('heading',{name:'调用追踪',level:2,exact:true}).waitFor();
    assert.equal(await page.evaluate(()=>location.hash),'#/traces?status=error&range=hour&model=claude-opus-5-5');
    await page.locator('tbody tr').filter({hasText:'claude-opus-5-5'}).first().waitFor();
    assert.equal(await page.locator('tbody tr').count(),7);
    // 查看模型: 模型与定价 scrolled to it and marked; the address names it, so a reload shows it again.
    await nav('运营概览');await failing.getByRole('button',{name:'查看模型',exact:true}).click();
    await page.locator('tr[data-model="claude-opus-5-5"].is-marked').waitFor();
    assert.equal(await page.evaluate(()=>location.hash),'#/models?model=claude-opus-5-5');
    await page.reload();await page.locator('tr[data-model="claude-opus-5-5"].is-marked').waitFor();
    await nav('运营概览');
    assert.equal(await attention.locator('li').filter({hasText:'claude-opus-4-8 近 24 小时'}).getByRole('button',{name:'查看模型'}).count(),0,'a model not listed has no model to open');
    console.log('PASS: 需要关注 names the models that fail (in the last hour, or all of the last day) and opens their failed requests or, when listed, the model; 服务健康 counts attempts, failures, takeovers and kinds');

    // The provider cards and their Keys: the same attempts, per Key too.
    await nav('供应商与 Key');
    const card=page.locator('.provider-card').filter({hasText:'瀚月 Max'});
    assert.equal(await card.getByLabel('近 24 小时尝试').innerText(),'近 24 小时 尝试 12 · 失败 12（100.0%） · 被备用接管 5 · HTTP 529 · 上游过载 × 12');
    const keyRow=id=>page.getByRole('row').filter({has:page.getByText(id,{exact:true})});
    assert.equal(await keyRow('hanyue-max-key-1').locator('.col-key-traffic').innerText(),'12 次 · 失败 100.0%');
    assert.equal(await keyRow('fixture-key').locator('.col-key-traffic').innerText(),'56 次 · 失败 14.3%');
    assert.equal(await keyRow('fixture-openai-key-1').locator('.col-key-traffic').innerText(),'—','no attempts');
    console.log('PASS: provider cards show their attempts, failures, takeovers and kinds, and each Key its 24-hour attempts and failure rate');

    // An older server reports none of this: the traces the console has are counted the same way.
    await page.route('**/api/v1/admin/stats',async route=>{const response=await route.fetch();const body=await response.json();
      for(const field of ['providerAttempts','keyAttempts','modelHealth','modelUsage7d'])delete body.activity[field];await route.fulfill({json:body});});
    await button('刷新').click();await page.locator('.btn-refresh:not([disabled])').waitFor();
    assert.equal(await card.getByLabel('近 24 小时尝试').innerText(),'近 24 小时 尝试 12 · 失败 12（100.0%） · 被备用接管 5 · HTTP 529 · 上游过载 × 12');
    await nav('运营概览');
    assert.deepEqual((await cells('瀚月 Max')).slice(3,8),['12','12（100.0%）','—','5','HTTP 529 · 上游过载 × 12']);
    await health.locator('.estimate-tag').waitFor();
    await attention.locator('li').filter({has:page.getByText('近 1 小时 claude-opus-5-5 失败 7 次',{exact:true})}).getByRole('button',{name:'查看失败请求',exact:true}).waitFor();
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log('PASS: without the server\'s attempts, the traces are counted the same way (marked as an estimate), and failed requests of the last hour are named as before');
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
