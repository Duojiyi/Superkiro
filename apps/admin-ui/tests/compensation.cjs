// 补偿这次扣费: from a request's drawer, or requests ticked in a card's 最近调用, 调账 opens with the charge,
// a reason naming the request and the request it makes up for; the card's history leads back to it,
// and a retry names the same request. The server refuses a request compensated already, or by more
// than it charged, saying what it found; 仍要补偿 sends it with a reason. Final build + loopback
// fixture, never production.
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
// fixture-trace-19 was cut off mid-output (输出中断) and still charged 3.2 积分, on fixture-card-0; 20 and 21
// charged it too, 1.5 积分 each.
const broken=fixture.traces[19];
Object.assign(broken,{card_id:'fixture-card-0',invocation_id:'fixture-card-0:fixture-inv-19',credits_charged:3200000});
for(const i of [20,21])Object.assign(fixture.traces[i],{card_id:'fixture-card-0',invocation_id:`fixture-card-0:fixture-inv-${i}`,credits_charged:1500000});
// As the console lists times: today HH:mm:ss, other days MM-DD HH:mm.
const listTime=secs=>{const d=new Date(secs*1000),p=n=>String(n).padStart(2,'0'),today=d.toDateString()===new Date().toDateString();
  return today?`${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`:`${p(d.getMonth()+1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;};
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
    const heading=name=>page.getByRole('heading',{name,level:2,exact:true}).waitFor();
    const go=hash=>page.evaluate(value=>{location.hash=value;},hash);
    const traceDrawer=page.locator('#trace-detail'),cardDrawer=page.locator('#card-detail');
    const adjust=page.getByRole('dialog',{name:'卡密调账'});
    const amount=adjust.getByRole('spinbutton',{name:'增减积分数量'}),reason=adjust.getByRole('textbox',{name:'调账原因说明'});
    const adjusts=()=>fixture.writes.filter(write=>write.endpoint==='cards/adjust').map(write=>write.body);
    const refusal=adjust.getByRole('alert',{name:'服务器拒绝补偿'});
    const compensate=async i=>{await go(`#/traces?card=fixture-card-0&open=fixture-trace-${i}`);await traceDrawer.getByRole('button',{name:'补偿这次扣费',exact:true}).click();await adjust.waitFor();};
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();await button('刷新').waitFor();

    // A request that charged nothing has nothing to give back.
    await go('#/traces?card=fixture-card-0&open=fixture-trace-18');await traceDrawer.getByText('上游未响应（未开始输出）',{exact:true}).first().waitFor();
    assert.equal(await traceDrawer.getByRole('button',{name:'补偿这次扣费',exact:true}).count(),0);
    // A charged one: 调账 on 卡密资产, with the charge, a reason naming it, and the request it makes up for.
    await go('#/traces?card=fixture-card-0&open=fixture-trace-19');await traceDrawer.getByText('输出中断',{exact:true}).first().waitFor();
    await traceDrawer.getByRole('button',{name:'补偿这次扣费',exact:true}).click();
    await heading('卡密资产');await adjust.waitFor();await cardDrawer.waitFor();
    const expected=`补偿 ${listTime(broken.ts)} gpt-5（输出中断）`;
    assert.deepEqual([await amount.inputValue(),await reason.inputValue()],['3.2',expected]);
    await adjust.getByText('补偿这次请求扣的 3.2 积分',{exact:true}).waitFor();
    await button('下一步').click();
    const review=await adjust.getByLabel('调账复核').innerText();
    assert(review.includes(`原因：${expected}`)&&review.includes('关联请求：')&&review.includes('可从记录打开这次请求'),review);
    await button('确认入账').click();await adjust.waitFor({state:'detached'});
    await toasts.shown('已调整 fixture-card-0：+3.2 积分');
    const sent=adjusts().at(-1);
    assert.deepEqual({...sent,idempotencyKey:undefined},{cardId:'fixture-card-0',deltaPoints:3.2,reason:expected,idempotencyKey:undefined,invocationId:'fixture-card-0:fixture-inv-19'});
    // The card's history leads back to the request.
    const first=cardDrawer.getByRole('region',{name:'操作记录'}).locator('tbody tr').first();
    await first.filter({hasText:'调账'}).filter({hasText:'+3.2'}).waitFor();
    await first.getByRole('button',{name:'查看请求',exact:true}).click();
    await heading('调用追踪');await traceDrawer.getByText('输出中断',{exact:true}).first().waitFor();
    assert.equal(await page.evaluate(()=>location.hash),'#/traces?card=fixture-card-0&open=fixture-trace-19');
    await page.keyboard.press('Escape');await traceDrawer.waitFor({state:'detached'});
    // An address can name the request by its invocation ID, as a card's history does for older requests.
    await go('#/traces?card=fixture-card-0&open=fixture-card-0%3Afixture-inv-19');
    await traceDrawer.getByText('输出中断',{exact:true}).first().waitFor();
    assert.equal(await page.evaluate(()=>location.hash),'#/traces?card=fixture-card-0&open=fixture-trace-19','the request found is the one opened');
    console.log('PASS: 补偿这次扣费 opens 调账 with the charge, a reason naming the request and its link; the history leads back to the request, also by invocation ID');

    // Compensated already: the server says when, by whom and why, and nothing is given; 仍要补偿 asks why
    // and sends it with allowRepeat, the reason kept with why.
    // The server's refusal object is read before its words: here the object names another operator.
    await page.route('**/api/v1/admin/cards/adjust',async route=>{const response=await route.fetch(),json=await response.json();
      if(json.refusal)json.refusal.requests[0].operator='值班员';await route.fulfill({response,json});},{times:1});
    await compensate(19);await button('下一步').click();await button('确认入账').click();
    await refusal.getByText('这次请求已经补偿过，没有入账',{exact:true}).waitFor();
    const found=await refusal.innerText();
    assert(found.includes(`这次请求 ${listTime(broken.ts)} 扣了 3.2 积分`)&&found.includes(`由 值班员 补偿过 3.2 积分（原因：${expected}）`),found);
    assert.equal(await button('确认入账').count(),0,'the same adjustment would be refused again');
    const facts=await (async()=>{await button('仍要补偿…').click();const box=page.getByRole('alertdialog');await box.waitFor();const text=await box.innerText();
      assert(await box.locator('[data-confirm="accept"]').isDisabled(),'why is required first');await box.locator('#confirm-reason').fill('上次补偿后又失败了');
      await box.locator('[data-confirm="accept"]').click();await box.waitFor({state:'detached'});return text;})();
    assert(facts.includes('仍要补偿这次请求？')&&facts.includes(`由 值班员 补偿过 3.2 积分`),facts);
    await adjust.waitFor({state:'detached'});await toasts.shown('已调整 fixture-card-0：+3.2 积分');
    const again=adjusts().at(-1);
    assert.deepEqual([again.allowRepeat,again.invocationId,again.reason],[true,'fixture-card-0:fixture-inv-19',`${expected}；仍要补偿：上次补偿后又失败了`]);
    await cardDrawer.getByRole('region',{name:'操作记录'}).locator('tbody tr').first().filter({hasText:'仍要补偿：上次补偿后又失败了'}).waitFor();
    // More than it charged: refused until the amount is changed back.
    await compensate(20);await amount.fill('5');await button('下一步').click();await button('确认入账').click();
    await refusal.getByText('补偿多于这次请求扣的积分，没有入账',{exact:true}).waitFor();
    assert((await refusal.innerText()).includes('这次要补偿 5 积分，多于它扣的'));
    await button('返回修改').click();assert.equal(await refusal.count(),0,'changing the adjustment clears the refusal');
    await amount.fill('1.5');await button('下一步').click();await button('确认入账').click();await adjust.waitFor({state:'detached'});
    assert.equal(adjusts().at(-1).allowRepeat,undefined);
    // A request the server cannot find (its trace pruned meanwhile) can only be looked into, not repeated.
    await compensate(21);const pruned=fixture.traces.splice(21,1)[0];
    await button('下一步').click();await button('确认入账').click();
    await refusal.getByText('服务器找不到这次请求，没有入账',{exact:true}).waitFor();
    assert.equal(await button('仍要补偿…').count(),0);assert.equal(await button('确认入账').count(),0);
    await adjust.getByRole('button',{name:'返回修改',exact:true}).click();await adjust.getByRole('button',{name:'取消',exact:true}).click();await adjust.waitFor({state:'detached'});
    fixture.traces.splice(21,0,pruned);
    console.log('PASS: a request compensated already or by more than it charged is refused with what the server found (its refusal object first), and 仍要补偿 sends it with why; an unknown one cannot be repeated');

    // Several requests ticked in 最近调用: their charges added up, named in the reason, linked to none.
    await nav('卡密资产');await page.getByRole('row').filter({has:page.getByLabel('选择卡密 fixture-card-0',{exact:true})}).locator('.col-group').click();
    const recent=cardDrawer.getByRole('region',{name:'最近调用'});await recent.locator('tbody tr').first().waitFor();
    const box=time=>recent.getByRole('checkbox',{name:`补偿 ${time} 的请求`,exact:true});
    const fmt=secs=>{const d=new Date(secs*1000),p=n=>String(n).padStart(2,'0');return d.toDateString()===new Date().toDateString()?`${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`:`${d.getFullYear()}-${p(d.getMonth()+1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;};
    assert(await box(fmt(fixture.traces[18].ts)).isDisabled(),'a request that charged nothing cannot be ticked');
    await box(fmt(fixture.traces[0].ts)).check();await box(fmt(fixture.traces[6].ts)).check();
    assert.equal(await cardDrawer.locator('.drawer-head').getByText('fixture-card-0',{exact:true}).count(),1,'ticking does not open the request');
    await recent.getByRole('button',{name:'补偿选中的 2 次（3.0000 积分）',exact:true}).click();
    await adjust.waitFor();
    assert.equal(await amount.inputValue(),'3');
    assert.equal(await reason.inputValue(),`补偿 2 次请求：${listTime(fixture.traces[6].ts)} claude-sonnet、${listTime(fixture.traces[0].ts)} claude-sonnet`);
    await adjust.getByText('补偿 2 次请求扣的共 3 积分。一笔调账只能关联一次请求，这几次写在原因里',{exact:true}).waitFor();
    await button('下一步').click();const several=await adjust.getByLabel('调账复核').innerText();
    assert(!several.includes('关联请求')&&several.includes('这 2 次请求不关联到调账：服务器不能检查它们是否已经补偿过，请先核对这张卡的操作记录。'),several);
    await button('确认入账').click();await adjust.waitFor({state:'detached'});
    assert.equal('invocationId' in adjusts().at(-1),false);assert.equal(adjusts().at(-1).deltaPoints,3);
    console.log('PASS: requests ticked in 最近调用 open 调账 with their charges added up and named in the reason; one without a charge cannot be ticked');

    // A lost reply: the intent is kept with its request, and the retry names the same one, still 仍要补偿.
    await compensate(19);await button('下一步').click();await button('确认入账').click();await refusal.waitFor();
    let lost;await page.route('**/api/v1/admin/cards/adjust',route=>{lost=route.request().postDataJSON();return route.abort('failed');},{times:1});
    await button('仍要补偿…').click();await (async()=>{const box=page.getByRole('alertdialog');await box.locator('#confirm-reason').fill('上次补偿不足');await box.locator('[data-confirm="accept"]').click();await box.waitFor({state:'detached'});})();
    await adjust.getByRole('alert').filter({hasText:'没收到调账结果'}).waitFor();
    await adjust.getByText('这笔调账仍要补偿一次请求，重试时仍关联它',{exact:true}).waitFor();
    await button('确认入账').click();await adjust.waitFor({state:'detached'});
    assert.deepEqual(adjusts().at(-1),lost,'the retry sends the same key and the same request');
    assert.deepEqual([lost.invocationId,lost.allowRepeat,lost.reason],['fixture-card-0:fixture-inv-19',true,`${expected}；仍要补偿：上次补偿不足`]);
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log('PASS: an adjustment whose reply was lost is retried with the same key and the same linked request');
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
