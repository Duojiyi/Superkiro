// Requests left 进行中, the "/" and Enter keys, and layout at laptop and phone widths (the request
// drawer beside its list, 成本覆盖 with large counts, wide drawers). Final build + loopback fixture, never production.
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
// Three more requests still 进行中: two minutes, 29 minutes and 31 minutes old (the fixture's own is 11 hours old).
const running=(id,age)=>({id,card_id:'fixture-card-0',ts:now-age,invocation_id:`fixture-card-0:${id}`,exposed_model:'gpt-5',status:'in_progress',ttft_ms:null,tokens_per_second:null,
  error_class:null,provider_id:'fixture-provider',input_tokens:0,output_tokens:0,credits_charged:0,provider_cost_micro_cny:0,attempt_chain:[]});
fixture.traces.unshift(running('running-2m',120),running('running-29m',29*60),running('running-31m',31*60));
(async()=>{
  let browser;
  try{
    await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
    browser=await chromium.launch({headless:true,...(process.env.CHROME_PATH?{executablePath:process.env.CHROME_PATH}:{})});
    const page=await browser.newPage({viewport:{width:1280,height:900}}),errors=[];
    page.setDefaultTimeout(10000);
    const origin=`http://127.0.0.1:${server.address().port}`;
    page.on('pageerror',error=>{errors.push(error.message);console.error('Browser error:',error.message);});
    const nativeDialogs=[];page.on('dialog',dialog=>{nativeDialogs.push(dialog.message());void dialog.dismiss();});
    await page.route('**/*',route=>new URL(route.request().url()).origin===origin?route.continue():route.abort());
    const button=name=>page.getByRole('button',{name,exact:true});
    const nav=name=>page.getByRole('navigation').getByRole('button',{name,exact:true}).click();
    const focused=()=>page.evaluate(()=>document.activeElement?.getAttribute('aria-label'));
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();await button('刷新').waitFor();

    // 进行中 past 30 minutes is 可能已中断, with how long ago it started; the list and the drawer say so.
    await nav('调用追踪');
    await page.getByRole('tablist',{name:'状态筛选'}).locator('[data-value="in_progress"]').click();
    const traceRow=id=>page.locator('.traces-table tbody tr').filter({has:page.getByText(id,{exact:true})});
    await page.getByLabel('搜索调用记录',{exact:true}).fill('running-');
    assert.deepEqual((await page.locator('.traces-table tbody .col-result').allInnerTexts()).map(text=>text.replace(/\s+/g,' ')),['进行中','进行中','可能已中断 31 分钟前开始']);
    await page.getByLabel('搜索调用记录',{exact:true}).fill('');
    assert.equal(await page.locator('.traces-table tbody .col-result').filter({hasText:'可能已中断'}).count(),2,'the 31-minute one and the fixture\'s 11-hour one');
    await page.getByLabel('搜索调用记录',{exact:true}).fill('running-31m');
    // Enter opens the only match.
    await page.getByLabel('搜索调用记录',{exact:true}).press('Enter');
    const traceDrawer=page.locator('#trace-detail');await traceDrawer.waitFor();
    assert.equal(await traceDrawer.locator('.drawer-title .status-badge, .drawer-title [class*="badge"]').first().innerText(),'可能已中断');
    assert.equal(await traceDrawer.getByRole('note').innerText(),'31 分钟前开始，超过 30 分钟仍没有结果，可能已经中断（例如服务重启或连接断开）');
    await page.keyboard.press('Escape');await traceDrawer.waitFor({state:'detached'});
    console.log('PASS: a request 进行中 for more than 30 minutes reads 可能已中断 with its age, in the list and its drawer');

    // "/" puts the cursor in the page's search, not while typing and not under a dialog; Enter opens the only match.
    await page.getByLabel('搜索调用记录',{exact:true}).fill('');
    await page.getByRole('tablist',{name:'状态筛选'}).locator('[data-value="ALL"]').click();
    await page.locator('.page-title').click();await page.keyboard.press('/');
    assert.equal(await focused(),'搜索调用记录');
    await page.keyboard.type('fixture-trace-63');assert.equal(await page.getByLabel('搜索调用记录',{exact:true}).inputValue(),'fixture-trace-63','the "/" that moved the cursor is not typed');
    await page.keyboard.press('Enter');await traceDrawer.waitFor();
    assert((await traceDrawer.innerText()).includes('fixture-trace-63'));
    await page.keyboard.press('Escape');await traceDrawer.waitFor({state:'detached'});
    await nav('卡密资产');
    const cardSearch=page.getByLabel('搜索卡密',{exact:true});
    await page.locator('.page-title').click();await page.keyboard.press('/');
    assert.equal(await focused(),'搜索卡密');
    await page.keyboard.type('fixture');await page.keyboard.press('Enter');
    assert.equal(await page.locator('#card-detail').count(),0,'several matches: Enter opens nothing');
    await page.keyboard.type('-card-3');await page.keyboard.press('/');
    assert.equal(await cardSearch.inputValue(),'fixture-card-3/','inside a field "/" is just typed');
    await cardSearch.fill('fixture-card-3');await page.keyboard.press('Enter');
    const cardDrawer=page.locator('#card-detail');await cardDrawer.waitFor();
    assert((await cardDrawer.innerText()).includes('fixture-card-3'));
    await page.keyboard.press('Escape');await cardDrawer.waitFor({state:'detached'});
    await button('＋ 批量生成').click();const form=page.getByRole('dialog',{name:'批量生成卡密'});await form.waitFor();
    await form.getByRole('heading').click();await page.keyboard.press('/');
    assert.notEqual(await focused(),'搜索卡密','a dialog keeps the keys');
    await form.getByRole('button',{name:'取消',exact:true}).click();
    console.log('PASS: "/" focuses the page\'s search (not while typing, not under a dialog) and Enter opens the only card or request found');

    // At 1280 the request drawer sits beside the list and 结果 stays in view, also for a failed request's longer result.
    await nav('调用追踪');await cardSearch.waitFor({state:'detached'});
    await page.getByRole('tablist',{name:'状态筛选'}).locator('[data-value="ALL"]').click();
    const layout=async()=>page.evaluate(()=>{
      const drawer=document.querySelector('#trace-detail').getBoundingClientRect();
      const result=document.querySelector('.traces-table thead th:nth-child(4)').getBoundingClientRect();
      const scroll=document.querySelector('.traces-table').closest('.table-scroll');
      return {drawerLeft:drawer.left,drawerWidth:drawer.width,resultRight:result.right,scrolls:scroll.scrollWidth>scroll.clientWidth,overflow:document.documentElement.scrollWidth>innerWidth};
    });
    await page.locator('.traces-table tbody tr').filter({hasText:'失败'}).first().click();await traceDrawer.waitFor();
    for(const width of [1280,1440,1760,1920]){
      await page.setViewportSize({width,height:900});await page.waitForFunction(w=>innerWidth===w,width);
      const measured=await layout();
      assert(measured.resultRight<=measured.drawerLeft,`${width}: 结果 ends at ${measured.resultRight}, the drawer starts at ${measured.drawerLeft}`);
      assert.equal(measured.scrolls,false,`${width}: the list fits beside the drawer`);assert.equal(measured.overflow,false);
    }
    await page.setViewportSize({width:1280,height:900});await page.waitForFunction(()=>innerWidth===1280);
    assert.equal(Math.round((await layout()).drawerWidth),480,'min(620px, 100vw − 800px)');
    await page.keyboard.press('Escape');await traceDrawer.waitFor({state:'detached'});
    console.log('PASS: from 1280px the request drawer sits beside the list, which keeps its 结果 column in view');

    // 财务对账 with counts in the millions: no KPI is cut off; 成本覆盖's total may wrap under the count.
    await page.route('**/api/v1/admin/financials',async route=>{
      const response=await route.fetch();const data=await response.json();
      data.estimates={...data.estimates,costedRequests:12345678,uncostedRequests:1234,usageFaceValueMicroCny:987654321000,configuredProviderCostMicroCny:123456789000};
      data.dashboard={...data.dashboard,total_requests:12346912,total_credits_charged:98765432100000};
      await route.fulfill({json:data});
    });
    await nav('财务对账');await button('刷新').click();
    const coverage=page.locator('.kpi').filter({hasText:'成本覆盖'});await coverage.getByText('12,345,678',{exact:false}).waitFor();
    for(const width of [1280,1100,390]){
      await page.setViewportSize({width,height:900});await page.waitForFunction(w=>innerWidth===w,width);
      const clipped=await page.evaluate(()=>[...document.querySelectorAll('.kpi-value')].filter(value=>value.scrollWidth>value.clientWidth+1).map(value=>value.textContent));
      assert.deepEqual(clipped,[],`${width}: no KPI value is cut off`);
      assert.equal((await coverage.locator('.kpi-value').innerText()).replace(/\s+/g,' '),'12,345,678 / 12,346,912');
    }
    await page.setViewportSize({width:1280,height:900});await page.unroute('**/api/v1/admin/financials');
    console.log('PASS: 财务对账 KPIs with counts in the millions are not cut off at 1280, 1100 and 390 px; 成本覆盖 puts the total beside or under the count');

    // Wide drawers: min(960px, 100vw − 240px), the whole width on a phone.
    await nav('模型与定价');
    await page.getByRole('checkbox',{name:'选择 claude-sonnet',exact:true}).check();
    await page.getByRole('region',{name:'批量模型操作'}).getByRole('button',{name:'批量调价',exact:true}).click();
    const wide=page.locator('#bulk-price');await wide.waitFor();
    for(const [width,expected] of [[1440,960],[1100,860],[800,560],[390,390]]){
      await page.setViewportSize({width,height:900});await page.waitForFunction(w=>innerWidth===w,width);
      assert.equal(Math.round((await wide.boundingBox()).width),expected,`${width}: wide drawer width`);
    }
    await page.setViewportSize({width:1280,height:900});
    await page.keyboard.press('Escape');await wide.waitFor({state:'detached'});
    console.log('PASS: wide drawers take min(960px, 100vw − 240px), and the whole width on a phone');
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
  }finally{if(browser)await browser.close();await new Promise(resolve=>server.close(resolve));}
})().catch(error=>{console.error(error);process.exitCode=1;});
