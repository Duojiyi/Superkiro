// 卡密资产: a card past its date, the single-card 封禁, 显示卡密 and handing a new batch out.
// Final build + loopback fixture, never production.
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
const DAY=86400,now=Math.floor(Date.now()/1000);
// fixture-card-5 (active, 1,500 of 2,000 积分 left) ran out three days ago; the server still records it as active.
fixture.cards[5].validUntil=now-3*DAY-3600;
// A card with an ID as the server makes them, so its short ID is shortened (card-74f4…bdeb).
const LONG='card-74f48022ecefbdeb';
fixture.cards.push({id:LONG,codeRecoverable:true,status:'active',creditTotal:1000000000,creditUsed:0,availableCredits:1000000000,pointsTotal:1000,pointsAvailable:1000,
  boundDevices:['dev_5616aa7828'],maxDevices:1,activatedAt:now-DAY,validUntil:now+20*DAY,groupId:'fixture-group-0',note:'淘宝 9 月'});
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
    const row=id=>page.getByRole('row').filter({has:page.getByLabel(`选择卡密 ${id}`,{exact:true})});
    const tab=value=>page.getByRole('tablist',{name:'状态筛选'}).locator(`[data-value="${value}"]`);
    const menu=async id=>{await row(id).getByRole('button',{name:'更多操作'}).click();return page.getByRole('menuitem').allInnerTexts();};
    const statusWrites=()=>fixture.writes.filter(write=>write.endpoint==='cards/status');
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();await button('刷新').waitFor();

    // Past its date a card is 已到期: not in use, and its balance no longer usable.
    const inUse=page.locator('.kpi').filter({hasText:'在用卡密'});
    await inUse.getByText('2 张',{exact:true}).waitFor();
    assert((await inUse.innerText()).includes('未激活 1 · 可用 6,700 积分'),await inUse.innerText());
    await nav('卡密资产');await row('fixture-card-5').waitFor();
    assert.equal(await row('fixture-card-5').locator('.col-status').innerText(),'已到期');
    assert((await row('fixture-card-5').locator('.col-expiry').innerText()).includes('已过期'));
    assert.deepEqual([await tab('ACTIVE').locator('.tab-count').innerText(),await tab('EXPIRED').locator('.tab-count').innerText()],['2','2']);
    await tab('EXPIRED').click();assert.equal(await page.getByRole('checkbox',{name:/^选择卡密 /}).count(),2);
    await tab('ACTIVE').click();assert.equal(await row('fixture-card-5').count(),0);
    await tab('CURRENT').click();
    assert.deepEqual(await menu('fixture-card-5'),['封禁'],'an expired card cannot be frozen');await page.keyboard.press('Escape');
    await row('fixture-card-5').locator('.col-group').click();
    const drawer=page.locator('#card-detail');await drawer.waitFor();
    assert((await drawer.innerText()).includes('（已过期 3 天 · 1,500 积分已不可用）'),await drawer.innerText());
    await drawer.getByRole('button',{name:'显示卡密',exact:true}).waitFor();
    assert.equal(await drawer.getByRole('button',{name:'冻结',exact:true}).count(),0);
    await page.keyboard.press('Escape');await drawer.waitFor({state:'detached'});
    console.log('PASS: a card past its date counts as 已到期 in 在用卡密, usable balance, tabs and actions, and its drawer says how long ago and what is lost');

    // 封禁 one card: its short ID typed (the end of a shortened one), 冻结 suggested for a card in use.
    await menu(LONG);await page.getByRole('menuitem',{name:'封禁',exact:true}).click();
    let box=page.getByRole('alertdialog');await box.waitFor();
    const text=await box.innerText();
    for(const part of ['封禁卡密 card-74f4…bdeb？','只想暂停？用冻结，可随时解冻。','客户马上不能使用，也不会自动退款；之后可以解封（要填原因）。'])assert(text.includes(part),`ban confirmation mentions ${part}: ${text}`);
    assert.equal(await box.locator('.field-label b').innerText(),'bdeb');
    const accept=box.locator('[data-confirm="accept"]');
    assert(await accept.isDisabled());await box.getByLabel('确认输入').fill('74f4');assert(await accept.isDisabled(),'the wrong part of the ID keeps it disabled');
    await box.getByLabel('确认输入').fill('bdeb');assert(!(await accept.isDisabled()));
    await accept.click();await box.waitFor({state:'detached'});
    await row(LONG).locator('.col-status').getByText('已封禁',{exact:true}).waitFor();
    assert.deepEqual(statusWrites().map(write=>[write.body.cardId,write.body.action]),[[LONG,'ban']]);
    await menu('fixture-card-0');await page.getByRole('menuitem',{name:'封禁',exact:true}).click();
    box=page.getByRole('alertdialog');await box.waitFor();
    assert.equal(await box.locator('.field-label b').innerText(),'fixture-card-0','an ID shown whole is typed whole');
    await box.getByRole('button',{name:'取消',exact:true}).click();await box.waitFor({state:'detached'});
    assert.equal(statusWrites().length,1);
    console.log('PASS: a single-card 封禁 needs the card\'s short ID typed and suggests 冻结');

    // 显示卡密: named for what it does; the code can be shown again later, and that is recorded.
    await row('fixture-card-0').getByRole('button',{name:'显示卡密',exact:true}).click();
    const shown=page.getByRole('dialog',{name:'显示卡密',exact:true});await shown.waitFor();
    assert((await shown.innerText()).includes('之后可在列表里重新查看（会记录）。'));
    await shown.getByRole('button',{name:'关闭',exact:true}).click();await shown.waitFor({state:'detached'});

    // Handing a batch out: the summary is a sale price; the CSV carries plan, points, validity, group and
    // note; 复制发货文本 gives each code with its plan, validity and where to download the client.
    await page.evaluate(()=>{window.copied=[];Object.defineProperty(navigator,'clipboard',{configurable:true,value:{writeText:async value=>{window.copied.push(value);}}});});
    await button('＋ 批量生成').click();
    const form=page.getByRole('dialog',{name:'批量生成卡密'});await form.waitFor();
    await form.getByLabel('积分套餐',{exact:true}).selectOption('tier-1000');await form.getByLabel('模型与计费分组',{exact:true}).selectOption('fixture-group-1');
    await form.getByLabel('生成数量',{exact:true}).fill('2');await form.getByLabel('备注',{exact:true}).fill('淘宝 9 月');
    const summary=await form.getByLabel('发卡摘要').innerText();
    assert(summary.includes('合计 2,000 积分 · 售价合计 ¥60.00（按套餐价）'),summary);assert(!summary.includes('面值'));
    await form.locator('.modal-actions .btn-primary').click();
    await page.getByRole('alertdialog').locator('[data-confirm="accept"]').click();
    const done=page.getByRole('dialog',{name:'新生成的卡密'});await done.waitFor();
    // Every card issued keeps its code (issuing needs the server's key): once the list has them, it says so.
    await done.getByText('之后可在列表里重新查看（会记录）。',{exact:false}).waitFor();
    const downloadPromise=page.waitForEvent('download');await done.getByRole('button',{name:'下载 CSV',exact:true}).click();
    const download=await downloadPromise;assert(download.suggestedFilename().startsWith('generated-cards-'));
    const line=i=>`"fixture-issued-${7+i}","FIXTURE-NOT-VALID-1000-${i}","PRO","1000","激活后 30 天","PRO+","淘宝 9 月"`;
    assert.equal(fs.readFileSync(await download.path(),'utf8'),'\uFEFF"卡密 ID","卡密","套餐","积分","有效期","分组","备注"\r\n'+line(0)+'\r\n'+line(1));
    await done.getByRole('button',{name:'复制发货文本',exact:true}).click();
    await toasts.shown('已复制 2 张卡的发货文本');
    const handout=i=>`卡密：FIXTURE-NOT-VALID-1000-${i}\n套餐：PRO（1,000 积分）\n有效期：激活后 30 天\n下载地址：https://kiro.rent`;
    assert.deepEqual(await page.evaluate(()=>window.copied),[handout(0)+'\n\n'+handout(1)]);
    await done.getByRole('button',{name:'完成',exact:true}).click();
    box=page.getByRole('alertdialog');await box.waitFor();
    assert((await box.innerText()).includes('之后可在列表里重新查看（会记录）。'));
    await box.locator('[data-confirm="accept"]').click();await done.waitFor({state:'detached'});
    assert.equal(await page.evaluate(()=>JSON.stringify({...localStorage,...sessionStorage}).includes('FIXTURE-NOT-VALID')),false,'no code is kept in the browser');
    assert.equal(await page.evaluate(()=>location.href.includes('FIXTURE')),false);
    console.log('PASS: 显示卡密 and its note; the batch summary is a sale price; the CSV and 复制发货文本 carry what a buyer needs');
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
  }finally{if(browser)await browser.close();await new Promise(resolve=>server.close(resolve));}
})().catch(error=>{console.error(error);process.exitCode=1;});
