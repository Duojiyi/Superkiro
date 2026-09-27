// Requests refused for the card or the request itself (balance, a prompt too long, a model not listed) are
// 拒绝, apart from 失败: on 运营概览 (请求, the chart, 服务健康) and on 调用追踪 (their own tab, the totals,
// the breakdown), with 同类拒绝 ×N for a refusal that stands for the same one again within a minute.
// Final build + loopback fixture, never production.
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
const refusal=(id,card,ago,model,errorClass,extra={})=>({id,card_id:card,ts:now-ago,invocation_id:`${card}:${id}`,exposed_model:model,status:'error',ttft_ms:null,tokens_per_second:null,
  error_class:errorClass,provider_id:null,input_tokens:0,output_tokens:0,credits_charged:0,provider_cost_micro_cny:0,attempt_chain:[],...extra});
// Three refusals a few minutes ago: the balance (it stands for four more within the minute), a prompt the
// upstream refused as too long (its attempt was made), and a model not listed.
fixture.traces.splice(1,0,
  refusal('refused-balance','fixture-card-0',300,'claude-sonnet','insufficient_balance',{needed_micro_credits:20300000,available_micro_credits:15000000,repeats:4,last_seen_secs:now-250}),
  refusal('refused-long','fixture-card-5',400,'gpt-5','input_too_long',{provider_id:'fixture-provider',attempt_chain:[{provider_id:'fixture-provider',key_id:'fixture-key',success:false,error:'http_400',latency_ms:300}]}),
  refusal('refused-unlisted','fixture-card-5',500,'claude-opus-9','model_not_listed'));
const failedToday=fixture.traces.filter(t=>t.status==='error'&&!t.id.startsWith('refused-')&&t.ts>now-86400).length;
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
    const kpi=label=>page.locator('.kpi').filter({has:page.locator('.kpi-label',{hasText:label})});
    const tab=value=>page.getByRole('tablist',{name:'追踪状态筛选'}).locator(`[data-value="${value}"]`);
    const rows=page.locator('.traces-table tbody tr:not(.state-row)');
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();await button('刷新').waitFor();

    // 运营概览: 拒绝 beside 失败 (the refusal standing for five counts five), in the chart and, for the upstream's own refusal, in 服务健康.
    const requests=kpi('请求').locator('.kpi-sub');await requests.getByRole('button',{name:'拒绝 7',exact:true}).waitFor();
    assert((await requests.innerText()).includes(`失败 ${failedToday}`),await requests.innerText());
    assert((await page.locator('.request-chart').getAttribute('aria-label')).endsWith('另有拒绝 7 次'));
    const health=page.getByRole('region',{name:'服务健康'}).getByRole('row').filter({hasText:'测试供应商 / Fixture'});
    const headers=await page.getByRole('region',{name:'服务健康'}).locator('thead th').allInnerTexts();
    assert.equal((await health.locator('td').nth(headers.indexOf('拒绝')).innerText()),'1','the upstream refused the prompt too long: not a failure');
    console.log('PASS: 运营概览 counts 拒绝 apart from 失败 (a repeated refusal as many times as it stands for), in the chart too, and 服务健康 shows the attempt the upstream refused');

    // 调用追踪: 拒绝 is its own tab, apart from 失败 in the counts, the rows, the totals and the breakdown.
    await requests.getByRole('button',{name:'拒绝 7',exact:true}).click();
    await page.getByRole('heading',{name:'调用追踪',level:2,exact:true}).waitFor();
    assert.equal(await tab('refused').getAttribute('aria-selected'),'true');assert(new URL(page.url()).hash.includes('status=refused'),page.url());
    await page.waitForFunction(()=>document.querySelectorAll('.traces-table tbody tr:not(.state-row)').length===3);
    assert.deepEqual([await tab('refused').locator('.tab-count').innerText(),await tab('error').locator('.tab-count').innerText()],['3',String(fixture.traces.filter(t=>t.status==='error').length-3)]);
    const balance=rows.filter({hasText:'余额不足：需要 20.3 积分，余额 15 积分'});
    assert.equal(await balance.locator('.badge').innerText(),'拒绝');
    assert.equal(await balance.locator('.repeat-count').innerText(),'同类拒绝 ×5');
    assert((await balance.locator('.repeat-count').getAttribute('title')).startsWith('一分钟内这张卡因同一原因被拒绝了 5 次，只记这一条'));
    await page.getByRole('group',{name:'拒绝原因'}).getByRole('button',{name:/^余额不足/}).waitFor();
    const totals=page.getByRole('group',{name:'合计'});assert(!(await totals.innerText()).includes('失败 3'));
    // Its details: 拒绝原因, the repeats, and nothing kept to read as nothing was sent.
    await balance.click();const drawer=page.locator('#trace-detail');
    await drawer.getByText('拒绝原因',{exact:true}).waitFor();await drawer.getByText('同类拒绝 ×5',{exact:true}).waitFor();
    await drawer.getByText('这次在发给上游之前就被拒绝，没有请求内容',{exact:true}).waitFor();
    await page.keyboard.press('Escape');await drawer.waitFor({state:'detached'});
    // 失败 holds the failures only; its breakdown names no refusal.
    await tab('error').click();await page.waitForFunction(count=>document.querySelectorAll('.traces-table tbody tr:not(.state-row)').length===count,fixture.traces.filter(t=>t.status==='error').length-3);
    assert.equal(await rows.locator('.badge',{hasText:'拒绝'}).count(),0);
    assert(!(await page.getByRole('group',{name:'失败按模型'}).innerText()).includes('claude-opus-9'));
    // 全部: the totals say 失败 and 拒绝 apart.
    await tab('ALL').click();await totals.getByText(/^拒绝/).waitFor();
    const words=(await totals.innerText()).replace(/\s+/g,' ');
    assert(words.includes(`失败 ${fixture.traces.filter(t=>t.status==='error').length-3}`)&&words.includes('拒绝 3'),words);
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log('PASS: 调用追踪 lists 拒绝 under its own tab with its reasons, 同类拒绝 ×5 on a repeated refusal, apart from 失败 in the counts, the totals and the breakdown');
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
