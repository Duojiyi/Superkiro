// A card's drawer: 限额 (requests at once, credits a UTC day and over 30 days), each change written to
// the card's history with the values it replaced; 更换卡密 behind a strong confirmation, the new code
// shown once; refusals in words. Final build + loopback fixture, never production.
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

    // 更换卡密: in the danger area, behind a typed confirmation with a reason; the new code is shown once,
    // with the text for the customer; the history keeps fingerprints only; 显示卡密 then shows the new code.
    await page.evaluate(()=>{window.copied=[];Object.defineProperty(navigator,'clipboard',{configurable:true,value:{writeText:async value=>{window.copied.push(value);}}});});
    await page.keyboard.press('Escape');await drawer.waitFor({state:'detached'});await open('fixture-card-5');
    const danger=drawer.getByRole('region',{name:'危险操作'});
    await danger.getByRole('button',{name:'更换卡密',exact:true}).click();
    const confirm=page.getByRole('alertdialog');await confirm.waitFor();
    const asked=await confirm.innerText();
    assert(asked.includes('更换卡密 fixture-card-5？')&&asked.includes('旧卡密马上失效，这张卡的所有登录马上退出，客户要用新卡密重新登录')&&asked.includes('余额、有效期、设备和记录都不变'),asked);
    const accept=confirm.locator('[data-confirm="accept"]');
    await confirm.getByRole('button',{name:'卡密泄露',exact:true}).click();assert(await accept.isDisabled(),'the card ID must be typed');
    await confirm.getByLabel('确认输入',{exact:true}).fill('fixture-card-5');await accept.click();await confirm.waitFor({state:'detached'});
    const shown=page.getByRole('dialog',{name:'新卡密'});await shown.waitFor();await toasts.shown('已更换 fixture-card-5 的卡密');
    const code=await shown.getByLabel('新卡密明文',{exact:true}).inputValue();
    assert.match(code,/^kiro(-[0-9a-f]{4}){8}$/);assert.equal(code,fixture.cards[5].rawCode);
    assert.deepEqual(writes('cards/rekey'),[{cardId:'fixture-card-5',reason:'卡密泄露'}]);
    assert((await shown.innerText()).includes('以后按新卡密搜索找不到这张卡，请用卡密 ID 或备注搜索'));
    await shown.getByRole('button',{name:'复制发货文本',exact:true}).click();await toasts.shown('已复制发给客户的文本');
    const text=(await page.evaluate(()=>window.copied)).at(-1);
    assert(text.startsWith(`卡密：${code}\n`)&&text.includes('余额：1,500 积分')&&text.endsWith('原来的卡密已停用，请用新卡密重新登录。'),text);
    // Closing asks whether it was kept; until then leaving the page asks too.
    await shown.getByRole('button',{name:'完成',exact:true}).click();const kept=page.getByRole('alertdialog');await kept.waitFor();
    assert((await kept.innerText()).includes('之后可在列表里用“显示卡密”再看（会记录）'));await kept.locator('[data-confirm="accept"]').click();await shown.waitFor({state:'detached'});
    await history().first().filter({hasText:'更换卡密'}).filter({hasText:'卡密泄露'}).waitFor();
    const entry=await history().first().innerText();assert(/卡密指纹 [0-9a-f]{8} → [0-9a-f]{8}/.test(entry)&&!entry.includes(code),entry);
    await idle();
    await drawer.getByRole('button',{name:'显示卡密',exact:true}).click();
    assert.equal(await page.getByRole('dialog',{name:'显示卡密'}).getByLabel('卡密明文',{exact:true}).inputValue(),code);
    await page.getByRole('dialog',{name:'显示卡密'}).getByRole('button',{name:'关闭',exact:true}).click();
    // An archived card must be unarchived first: said on the button; a refusal from the server is said in words.
    await page.route('**/api/v1/admin/cards/rekey',route=>route.fulfill({status:409,contentType:'application/json',body:JSON.stringify({success:false,error:'Archived cards must be unarchived before they are given a new code: fixture-card-5'})}),{times:1});
    await danger.getByRole('button',{name:'更换卡密',exact:true}).click();await confirm.waitFor();
    await confirm.locator('#confirm-reason').fill('客户要求');await confirm.getByLabel('确认输入',{exact:true}).fill('fixture-card-5');await accept.click();
    await page.getByRole('alert').filter({hasText:'没有更换卡密：已归档的卡要先取消归档，再更换卡密'}).waitFor();
    assert.equal(fixture.cards[5].rawCode,code,'nothing changed');assert.equal(await page.getByRole('dialog',{name:'新卡密'}).count(),0);
    console.log('PASS: 更换卡密 is in the drawer\'s danger area behind a typed confirmation with a reason; the new code is shown once with the text for the customer, the history keeps fingerprints only, and a refusal is said in words');

    // A card banned while it was frozen is frozen again when the ban is lifted, and says so.
    await page.keyboard.press('Escape');await drawer.waitFor({state:'detached'});await open('fixture-card-0');
    const reasonAndAccept=async reason=>{const box=page.getByRole('alertdialog');await box.waitFor();await box.locator('#confirm-reason').fill(reason);
      const typed=box.getByLabel('确认输入',{exact:true});if(await typed.count())await typed.fill('fixture-card-0');await box.locator('[data-confirm="accept"]').click();await box.waitFor({state:'detached'});};
    await drawer.getByRole('button',{name:'冻结',exact:true}).click();await reasonAndAccept('客户要求');await toasts.shown('已冻结 fixture-card-0');
    await drawer.locator('.drawer-head').getByText('已冻结',{exact:true}).waitFor();await idle();
    await drawer.getByRole('button',{name:'封禁',exact:true}).click();await reasonAndAccept('滥用');await drawer.locator('.drawer-head').getByText('已封禁',{exact:true}).waitFor();await idle();
    await drawer.getByRole('button',{name:'解封',exact:true}).click();
    const unban=page.getByRole('alertdialog');await unban.waitFor();assert((await unban.innerText()).includes('封禁前已冻结的仍是冻结（要再解冻）'));
    await reasonAndAccept('误封，已核实');
    await toasts.shown('已解封 fixture-card-0：已恢复为冻结，需要解冻才能使用');
    await drawer.locator('.drawer-head').getByText('已冻结',{exact:true}).waitFor();
    await history().first().filter({hasText:'解封'}).filter({hasText:'恢复为已冻结'}).waitFor();
    await drawer.getByRole('button',{name:'解冻',exact:true}).waitFor();
    // 运营概览 names the cards past their validity (the server's count), and leads to them.
    await page.keyboard.press('Escape');await drawer.waitFor({state:'detached'});
    await page.getByRole('navigation').getByRole('button',{name:'运营概览',exact:true}).click();
    const inUse=page.locator('.kpi').filter({has:page.locator('.kpi-label',{hasText:'在用卡密'})});
    await inUse.getByRole('button',{name:'已到期 1',exact:true}).click();
    await page.getByRole('tablist',{name:'状态筛选'}).locator('[data-value="EXPIRED"][aria-selected="true"]').waitFor();
    await row('fixture-card-4').waitFor();
    console.log('PASS: 解封 of a card banned while frozen says it is frozen again and must be unfrozen, and its history says so; 运营概览 counts the cards past their validity and opens them');

    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
