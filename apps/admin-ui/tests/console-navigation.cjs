// Page addresses and finding a card from its code: final build + loopback fixture, never production.
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
// A customer's card, issued from this code (its ID is the one the billing crate derives from it).
const CODE='kiro-3eba-7810-bb00-a2e3-465a-fcc8-b9de-7471',CARD='card-b802b003e19a4e48',HEX='3eba7810bb00a2e3465afcc8b9de7471';
const MISSING='kiro-0000-0000-0000-0000-0000-0000-0000-0001',MISSING_CARD='card-b48b34bd2116ba5a';
const now=Math.floor(Date.now()/1000);
fixture.cards.push({id:CARD,codeRecoverable:true,status:'active',creditTotal:2000000000,creditUsed:1000000000,availableCredits:1000000000,pointsTotal:2000,pointsAvailable:1000,
  boundDevices:['dev_5616aa7828'],maxDevices:1,activatedAt:now-86400*7,validUntil:now+86400*23,groupId:'fixture-group-1',note:'闲鱼 9 月'});
fixture.traces.unshift({id:'complaint-trace',card_id:CARD,ts:now-120,invocation_id:`${CARD}:complaint`,exposed_model:'gpt-5',status:'error',ttft_ms:null,tokens_per_second:null,
  error_class:'upstream_start_failed',provider_id:'fixture-provider',input_tokens:0,output_tokens:0,credits_charged:0,provider_cost_micro_cny:0,
  attempt_chain:[{provider_id:'fixture-provider',key_id:'fixture-key',success:false,error:'HTTP 529 overloaded_error: Overloaded',latency_ms:1200}]});
(async()=>{
  let browser;
  try{
    await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
    browser=await chromium.launch({headless:true,...(process.env.CHROME_PATH?{executablePath:process.env.CHROME_PATH}:{})});
    const context=await browser.newContext({viewport:{width:1440,height:1000}});
    const page=await context.newPage(),errors=[],sent=[];
    page.setDefaultTimeout(10000);
    const origin=`http://127.0.0.1:${server.address().port}`;
    page.on('pageerror',error=>{errors.push(error.message);console.error('Browser error:',error.message);});
    const nativeDialogs=[];page.on('dialog',dialog=>{nativeDialogs.push(dialog.message());void dialog.dismiss();});
    // Every request the console makes, with its body: the code must never be among them.
    await context.route('**/*',route=>{const request=route.request();sent.push(`${request.url()} ${request.postData()??''}`);return new URL(request.url()).origin===origin?route.continue():route.abort();});
    const button=name=>page.getByRole('button',{name,exact:true});
    const nav=name=>page.getByRole('navigation').getByRole('button',{name,exact:true}).click();
    const hash=()=>page.evaluate(()=>location.hash);
    const heading=name=>page.getByRole('heading',{name,level:2,exact:true}).waitFor();
    const back=()=>page.evaluate(()=>history.back()),forward=()=>page.evaluate(()=>history.forward());
    const login=async()=>{await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();await button('刷新').waitFor();};
    const answer=async accept=>{const box=page.getByRole('alertdialog');await box.waitFor();const text=await box.innerText();
      await (accept?box.locator('[data-confirm="accept"]'):box.getByRole('button',{name:'取消',exact:true})).click();await box.waitFor({state:'detached'});return text;};
    const leaks=async()=>{
      const kept=await page.evaluate(()=>JSON.stringify({...localStorage,...sessionStorage})+location.href+document.title+document.body.innerHTML);
      assert(!kept.includes(HEX.slice(0,8))&&!kept.includes('3eba-7810'),'the code is not kept in storage, the address or the page');
      assert(!sent.some(line=>line.includes(HEX.slice(0,8))||line.includes('3eba-7810')),'the code is never sent');
    };
    await page.goto(origin+'/admin/');await login();
    assert.equal(await page.title(),'运营概览 · Superkiro');

    // 卡密资产: the customer's code, in capitals with spaces, finds the card and opens it.
    await nav('卡密资产');assert.equal(await page.title(),'卡密资产 · Superkiro');assert.equal(await hash(),'#/cards');
    const search=page.getByLabel('搜索卡密',{exact:true});
    assert.equal(await search.getAttribute('placeholder'),'卡密原文、卡密 ID、备注或设备 ID');
    await search.fill(CODE.toUpperCase().replace(/-/g,' '));
    await page.getByRole('status').filter({hasText:'按卡密找到 1 张'}).waitFor();
    assert.equal(await search.inputValue(),CARD,'the card ID takes the place of the code');
    const drawer=page.locator('#card-detail');await drawer.locator('.drawer-head').getByText(CARD,{exact:true}).waitFor();
    assert.equal(await hash(),`#/cards?q=${CARD}&open=${CARD}`);
    await leaks();
    // A code no card has says so, with the card ID it would have.
    await search.fill(MISSING);
    await page.getByRole('status').filter({hasText:`按卡密没有找到：没有卡密 ID 为 ${MISSING_CARD} 的卡`}).waitFor();
    await page.getByText('没有匹配的卡密',{exact:true}).waitFor();
    console.log('PASS: a card code (any case, spaces) finds the card and opens it, or says no card has it; the code is never shown, sent or kept');

    // The address restores the page, its filters and the open card after a reload; Back and Forward
    // step through opening and closing it, then back to the previous page.
    await search.fill(CARD);await page.getByRole('row').filter({hasText:'闲鱼 9 月'}).locator('td.num').first().click();
    await drawer.locator('.drawer-head').getByText(CARD,{exact:true}).waitFor();
    await page.getByRole('tablist',{name:'状态筛选'}).locator('[data-value="ACTIVE"]').click();
    assert.equal(await hash(),`#/cards?q=${CARD}&status=ACTIVE&open=${CARD}`);
    await page.reload();await heading('卡密资产');
    await drawer.locator('.drawer-head').getByText(CARD,{exact:true}).waitFor();
    assert.equal(await search.inputValue(),CARD);
    assert.equal(await page.getByRole('tablist',{name:'状态筛选'}).locator('[aria-selected="true"]').getAttribute('data-value'),'ACTIVE');
    await page.keyboard.press('Escape');await drawer.waitFor({state:'detached'});
    assert.equal(await hash(),`#/cards?q=${CARD}&status=ACTIVE`);
    await back();await drawer.locator('.drawer-head').getByText(CARD,{exact:true}).waitFor();
    await forward();await drawer.waitFor({state:'detached'});
    await nav('调用追踪');await heading('调用追踪');assert.equal(await page.title(),'调用追踪 · Superkiro');
    await back();await heading('卡密资产');assert.equal(await search.inputValue(),CARD);
    await forward();await heading('调用追踪');
    console.log('PASS: reload restores the page, filters and open card; Back and Forward open and close it and move between pages; titles follow');

    // 调用追踪: the same code narrows the list to the card's requests.
    const traceSearch=page.getByLabel('搜索调用记录',{exact:true});
    assert.equal(await traceSearch.getAttribute('placeholder'),'卡密原文、卡密 ID 或请求 ID');
    await traceSearch.fill(` ${CODE} `);
    await page.getByText('按卡密找到 1 张',{exact:false}).waitFor();
    assert.equal(await traceSearch.inputValue(),CARD);
    await page.getByRole('row').filter({hasText:'上游未响应'}).first().waitFor();
    assert.equal(await page.locator('.traces-table tbody tr').count(),1);
    await page.getByRole('button',{name:'详情',exact:true}).click();await page.locator('#trace-detail').waitFor();
    assert.equal(await hash(),`#/traces?card=${CARD}&open=complaint-trace`);
    await page.reload();await page.locator('#trace-detail').waitFor();assert.equal(await traceSearch.inputValue(),CARD);
    await leaks();
    console.log('PASS: 调用追踪 finds a card from its code too; its address keeps the card and the open request');

    // Back with unpublished edits asks first: cancelled, nothing moves; agreed, the step is taken.
    await nav('模型与定价');await page.getByLabel('上下文长度',{exact:true}).fill('150000');
    await back();
    assert((await answer(false)).includes('有未发布的修改，确定离开？'));
    await heading('模型与定价');assert.equal(await hash(),'#/models');assert.equal(await page.getByLabel('上下文长度',{exact:true}).inputValue(),'150000');
    await back();await answer(true);await heading('调用追踪');
    console.log('PASS: Back asks before leaving unpublished edits, and stays when cancelled');

    // An address opened before signing in is kept for after the login; one Key is pointed out.
    const other=await browser.newPage({viewport:{width:1440,height:1000}});
    other.on('pageerror',error=>errors.push(error.message));
    await other.goto(origin+'/admin/#/providers?key=fixture-backup');
    await other.getByLabel('密码',{exact:true}).fill('fixture-password');await other.getByRole('button',{name:'登录',exact:true}).click();
    await other.getByRole('heading',{name:'供应商与 Key',level:2,exact:true}).waitFor();
    const pointed=other.locator('tr.is-pointed');await pointed.waitFor();
    assert.equal(await pointed.getAttribute('data-key-id'),'fixture-backup');
    assert(await pointed.evaluate(row=>{const box=row.getBoundingClientRect();return box.top>=0&&box.bottom<=innerHeight;}),'scrolled into view');
    // The Key open in the editor is part of the address too.
    await other.locator('.provider-card').filter({hasText:'OpenAI 格式 / Fixture'}).getByRole('button',{name:'编辑',exact:true}).click();
    await other.locator('#key-editor').getByRole('heading',{name:'编辑 Key · fixture-openai-key-1'}).waitFor();
    assert.equal(await other.evaluate(()=>location.hash),'#/providers?edit=fixture-openai-key-1');
    await other.reload();await other.locator('#key-editor').getByRole('heading',{name:'编辑 Key · fixture-openai-key-1'}).waitFor();
    await other.close();
    console.log('PASS: a bookmarked address survives the login; a Key named in it is marked and scrolled to; the open Key editor is restored');
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
