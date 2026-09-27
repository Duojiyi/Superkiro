// Requests the card's own balance or limits refused: 调用追踪 says why in words (余额不足：需要 X 积分，余额 Y),
// and the card's drawer says when its last refusal was for its balance. Final build + loopback fixture.
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
// A card with 15 积分 left: an Opus request needs about 20.3 to start (its reservation), so it was refused.
const LOW='card-3c9e5a7b1d2f4e60';
fixture.cards.push({id:LOW,codeRecoverable:true,status:'active',creditTotal:1000000000,creditUsed:985000000,availableCredits:15000000,pointsTotal:1000,pointsAvailable:15,
  boundDevices:['dev_77aa00bb11cc'],maxDevices:1,activatedAt:now-86400,validUntil:now+20*86400,groupId:'fixture-group-1',note:'闲鱼 9 月'});
// As the server records refusals before routing: no provider, no attempts, no charge, no content.
const refusal=(card,ts,errorClass,extra={})=>({id:`refused-${card}:inv-${ts}-1`,card_id:card,ts,invocation_id:`${card}:inv-${ts}`,exposed_model:'claude-opus-5',status:'error',
  ttft_ms:null,tokens_per_second:null,error_class:errorClass,provider_id:null,input_tokens:0,output_tokens:0,credits_charged:0,provider_cost_micro_cny:0,attempt_chain:[],...extra});
fixture.traces.unshift(refusal(LOW,now-30,'insufficient_balance',{needed_micro_credits:20300000,available_micro_credits:15000000}),
  refusal('fixture-card-2',now-40,'concurrency_limit'),refusal('fixture-card-5',now-50,'usage_limit'));
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
    const traceDrawer=page.locator('#trace-detail'),cardDrawer=page.locator('#card-detail');
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();await button('刷新').waitFor();

    // 调用追踪: under 失败, each refusal in words, the balance one with what it needed and what the card had.
    await nav('调用追踪');await page.getByRole('tablist',{name:'追踪状态筛选'}).locator('[data-value="error"]').click();
    const chips=await page.getByRole('group',{name:'失败原因'}).getByRole('button').allInnerTexts();
    for(const chip of ['余额不足 1','超过并发上限 1','超过每日或每月用量上限 1'])assert(chips.some(text=>text.replace(/\s+/g,' ').trim()===chip),`${chip}: ${chips.join(' | ')}`);
    const low=page.locator('tbody tr').filter({hasText:'card-3c9e…4e60'});
    assert.equal(await low.locator('.result-reason').innerText(),'余额不足：需要 20.3 积分，余额 15 积分');
    await low.getByRole('button',{name:'详情',exact:true}).click();
    const facts=await traceDrawer.locator('.detail-list').innerText();
    assert(facts.includes('余额不足：需要 20.3 积分，余额 15 积分')&&facts.includes('insufficient_balance'),facts);
    assert(facts.includes('请求开始前要按最大输出预留积分；余额不够预留，这次没有发给上游，也没有扣费。'),facts);
    await traceDrawer.getByText('没有记录尝试',{exact:true}).waitFor();
    assert.equal(await traceDrawer.getByRole('button',{name:'补偿这次扣费',exact:true}).count(),0,'nothing was charged');
    await page.getByRole('group',{name:'失败原因'}).getByRole('button',{name:/超过并发上限/}).click();
    // Beside the open drawer the list keeps 结果; a row opens its request.
    await page.locator('tbody tr').first().click();
    await traceDrawer.getByText('这张卡同时进行的请求已到上限，这次没有发给上游，也没有扣费。',{exact:true}).waitFor();
    await traceDrawer.getByText('这次在发给上游之前就被拒绝，没有请求内容',{exact:true}).waitFor();
    assert.equal(await traceDrawer.getByRole('button',{name:'查看内容（会记录）',exact:true}).count(),0,'no content to read');
    console.log('PASS: refusals for the card\'s balance, concurrency and usage limits read as words in the list, the reason chips and the drawer, with what a balance refusal needed');

    // The card's drawer says its last refusal was for its balance, while the balance is still short.
    await page.keyboard.press('Escape');await nav('卡密资产');
    await page.getByRole('row').filter({has:page.getByLabel(`选择卡密 ${LOW}`,{exact:true})}).locator('.col-group').click();
    const short=cardDrawer.locator('.balance-short');await short.waitFor();
    assert.equal(await short.innerText(),'余额不够开始 claude-opus-5（约需 20.3 积分）· 刚刚');
    // Topped up past what the request needed: nothing to say.
    Object.assign(fixture.cards.at(-1),{availableCredits:25000000,pointsAvailable:25});await button('刷新').click();await page.locator('.btn-refresh:not([disabled])').waitFor();
    await cardDrawer.getByText('25',{exact:true}).waitFor();assert.equal(await short.count(),0);
    // Short again, but refused since for its concurrency: the last refusal was not for the balance.
    Object.assign(fixture.cards.at(-1),{availableCredits:15000000,pointsAvailable:15});fixture.traces.unshift(refusal(LOW,now-5,'concurrency_limit'));
    await page.keyboard.press('Escape');await button('刷新').click();await page.locator('.btn-refresh:not([disabled])').waitFor();
    await page.getByRole('row').filter({has:page.getByLabel(`选择卡密 ${LOW}`,{exact:true})}).locator('.col-group').click();
    await cardDrawer.getByRole('region',{name:'最近调用'}).locator('tbody tr').first().waitFor();
    assert.equal(await short.count(),0);
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log('PASS: the card drawer says 余额不够开始 claude-opus-5（约需 20.3 积分） after a balance refusal, and not once topped up or refused since for something else');
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
