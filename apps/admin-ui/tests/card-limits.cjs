// A card's drawer: 限额 (requests at once, credits a UTC day and over 30 days), each change written to
// the card's history with the values it replaced; refusals in words. Final build + loopback fixture,
// never production.
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
    const idle=()=>page.locator('.btn-refresh:not([disabled])').waitFor();
    const row=id=>page.getByRole('row').filter({has:page.getByLabel(`选择卡密 ${id}`,{exact:true})});
    const drawer=page.locator('#card-detail');
    const open=async id=>{await row(id).locator('.col-group').click();await drawer.locator('.drawer-head').getByText(id,{exact:true}).waitFor();};
    const history=()=>drawer.getByRole('region',{name:'操作记录'}).locator('tbody tr');
    const writes=endpoint=>fixture.writes.filter(write=>write.endpoint===endpoint).map(write=>write.body);
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();await button('刷新').waitFor();
    await nav('卡密资产');await row('fixture-card-0').waitFor();

    // 限额: what the card allows now, in credits, with the windows said; nothing to save until something changes.
    await open('fixture-card-0');
    const limits=drawer.locator('.card-limits');
    await limits.getByText('同时 2 个请求 · 每日 不限 · 近 30 天 不限',{exact:true}).waitFor();
    assert((await limits.locator('span').first().getAttribute('title')).includes('每日按 UTC 日统计，北京时间每天 08:00 重置'));
    await limits.getByRole('button',{name:'修改',exact:true}).click();
    const dialog=page.getByRole('dialog',{name:'修改限额'});await dialog.waitFor();
    assert((await dialog.innerText()).includes('每日按 UTC 日统计，北京时间每天 08:00 重置；留空为不限'));
    const save=dialog.getByRole('button',{name:'保存限额',exact:true});
    assert(await save.isDisabled(),'nothing changed yet');await dialog.getByRole('status').getByText('没有修改',{exact:true}).waitFor();
    await dialog.getByLabel('同时请求数',{exact:true}).fill('21');
    await dialog.getByText('同时请求数须是 1–20 的整数',{exact:true}).waitFor();assert(await save.isDisabled());
    await dialog.getByLabel('同时请求数',{exact:true}).fill('4');await dialog.getByLabel('每日积分上限',{exact:true}).fill('500');
    const review=await dialog.getByRole('status').innerText();
    assert(review.includes('同时请求 2 → 4 个')&&review.includes('每日 不限 → 500 积分')&&!review.includes('近 30 天'),review);
    assert(await save.isDisabled(),'a reason is required');
    // A refusal is said in words and changes nothing: nothing to check afterwards.
    await page.route('**/api/v1/admin/cards/quotas',route=>route.fulfill({status:400,contentType:'application/json',body:JSON.stringify({success:false,error:'maxConcurrency must be between 1 and 20'})}),{times:1});
    await dialog.getByRole('button',{name:'防止滥用',exact:true}).click();await save.click();
    await dialog.getByRole('alert').filter({hasText:'同时请求数须在 1–20 之间'}).waitFor();
    assert.equal(await page.getByRole('region',{name:'卡密修改结果核对'}).count(),0);
    await save.click();await dialog.waitFor({state:'detached'});await toasts.shown('已修改 fixture-card-0 的限额');
    assert.deepEqual(writes('cards/quotas').at(-1),{cardId:'fixture-card-0',maxConcurrency:4,dailyCreditLimit:500000000,reason:'防止滥用'},'only what changed, in micro-credits');
    await limits.getByText('同时 4 个请求 · 每日 500 积分 · 近 30 天 不限',{exact:true}).waitFor();
    await history().first().filter({hasText:'修改限额'}).filter({hasText:'同时请求 2 → 4 个 · 每日 不限 → 500 积分'}).filter({hasText:'防止滥用'}).waitFor();
    await idle();
    // A limit left blank is none again.
    await limits.getByRole('button',{name:'修改',exact:true}).click();await dialog.waitFor();
    assert.equal(await dialog.getByLabel('每日积分上限',{exact:true}).inputValue(),'500');
    await dialog.getByLabel('每日积分上限',{exact:true}).fill('');await dialog.locator('#quota-reason').fill('客户要求放开');
    await save.click();await dialog.waitFor({state:'detached'});
    assert.deepEqual(writes('cards/quotas').at(-1),{cardId:'fixture-card-0',dailyCreditLimit:null,reason:'客户要求放开'});
    await history().first().filter({hasText:'每日 500 积分 → 不限'}).waitFor();
    await limits.getByText('同时 4 个请求 · 每日 不限 · 近 30 天 不限',{exact:true}).waitFor();
    await idle();
    console.log('PASS: 限额 shows requests at once and the daily (UTC day, 08:00 Beijing) and 30-day limits in credits; a change sends only what changed, needs a reason, is refused in words, and is in the history with the values replaced');

    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
