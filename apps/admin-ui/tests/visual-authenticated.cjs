// Authenticated, nonempty local visual and interaction checks. Fixture API is test-only.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
const http=require('node:http'), fs=require('node:fs'), path=require('node:path'), assert=require('node:assert/strict');
const fixture=require('./fixture-api.cjs')();
const root=path.resolve(__dirname,'../dist'), output=path.resolve(__dirname,'../visual-check/authenticated');
fs.mkdirSync(output,{recursive:true});
const errors=[];
const server=http.createServer(async(req,res)=>{
 try {
  if(req.url.startsWith('/api/')) return await fixture.handle(req,res);
  const relative=decodeURIComponent(req.url.split('?')[0]).replace(/^\/admin\/?/,'')||'index.html';
  const file=path.resolve(root,relative);
  if(!file.startsWith(root+path.sep)||!fs.existsSync(file)){res.writeHead(404);return res.end();}
  res.setHeader('Content-Type',file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':'text/html');res.end(fs.readFileSync(file));
 } catch(e){errors.push(e.message);res.writeHead(500);res.end(JSON.stringify({error:e.message}));}
});
(async()=>{
 await new Promise(r=>server.listen(0,'127.0.0.1',r));
 const browser=await chromium.launch({headless:true,...(process.env.CHROME_PATH?{executablePath:process.env.CHROME_PATH}:{})});
 try {
  const page=await browser.newPage({viewport:{width:1440,height:1080},deviceScaleFactor:1});
  page.on('pageerror',e=>errors.push(e.message)); page.on('dialog',d=>{errors.push('native dialog: '+d.message());void d.dismiss();});
  // Confirmations are the console's own dialog now.
  const confirm=async()=>{const box=page.getByRole('alertdialog');await box.waitFor();await box.locator('[data-confirm="accept"]').click();await box.waitFor({state:'detached'});};
  await page.route('**/*',route=>new URL(route.request().url()).origin===`http://127.0.0.1:${server.address().port}`?route.continue():route.abort());
  await page.goto(`http://127.0.0.1:${server.address().port}/admin/`);
  await page.getByLabel('密码',{exact:true}).waitFor();
  await page.locator('input[type=password]').fill('fixture-password');
  await page.getByRole('button',{name:'登录',exact:true}).click();
  await page.locator('.session-clock').filter({hasText:'admin · 剩余'}).waitFor();
  // Real 24-hour totals from /stats (fixture: 47 finished requests, 42 succeeded, 3 failed), not a trace sample.
  await page.locator('.kpi').filter({hasText:'成功率'}).getByText('89.4%',{exact:true}).waitFor();
  assert.equal(await page.locator('.kpi').filter({hasText:'请求'}).first().locator('strong').textContent(),'47');
  assert.equal(await page.locator('.chart-column').count(),24);
  const bars=await page.locator('.chart-column').evaluateAll(nodes=>nodes.map(n=>[Number(n.dataset.requests),Number(n.dataset.failed)]));
  assert(bars.reduce((a,[r])=>a+r,0)>0);assert.equal(bars.reduce((a,[,f])=>a+f,0),3);
  // A cooling Key of a disabled provider raises nothing: one badge, one attention item, for the enabled provider's Key.
  assert.equal(await page.locator('#nav-badge-providers [aria-hidden="true"]').textContent(),'1');
  const coolingItems=await page.locator('.attention-list li').filter({hasText:'冷却中'}).allInnerTexts();
  assert.equal(coolingItems.length,1);assert(coolingItems[0].startsWith('1 个 Key 冷却中'),coolingItems[0]);
  // Add screenshot-only provenance. No application code or production assets know about fixtures.
  await page.evaluate(()=>{const b=document.createElement('div');b.textContent='LOCAL FIXTURE · 已认证测试数据 · 非生产';Object.assign(b.style,{position:'fixed',bottom:'8px',right:'12px',zIndex:'9999',background:'#23272b',color:'white',padding:'6px 10px',fontSize:'11px',borderRadius:'4px',pointerEvents:'none'});document.body.append(b);});
  const pages=[['overview','运营概览'],['cards','卡密资产'],['providers','供应商与 Key'],['pricing','模型与定价'],['groups','分组与权益'],['trace','调用追踪'],['finance','财务对账'],['security','安全与审计'],['announcements','公告管理']];
  // Leaving a page with unpublished edits asks first; this walkthrough leaves them.
  const nav=async name=>{await page.getByRole('navigation').getByRole('button',{name,exact:true}).click();await page.waitForTimeout(150);if(await page.getByRole('alertdialog',{name:/确定离开/}).count())await confirm();await page.waitForLoadState('networkidle');await page.locator('.page-content').evaluate(e=>e.scrollTop=0);};
  const shot=async name=>{await page.locator('.toast').waitFor({state:'hidden'});await page.screenshot({path:path.join(output,name),fullPage:true});};
  for(const [id,name] of pages){
   await nav(name);
   if(id==='pricing'){
    // Each model shows its current price; an OpenAI-format model with 272K context reads 272K.
    const astra=page.getByRole('row').filter({hasText:'gpt-6-astra'}).filter({has:page.getByRole('button',{name:'调价'})});
    assert((await astra.innerText()).includes('272K / 128K'));assert((await astra.innerText()).includes('1.25 / 10'));
    await astra.getByRole('button',{name:'调价',exact:true}).click();
    const priceDrawer=page.locator('#price-drawer');await priceDrawer.waitFor();
    assert.equal(await priceDrawer.getByLabel('新输入售价',{exact:true}).inputValue(),'1.25');
    await priceDrawer.getByLabel('新输出售价',{exact:true}).fill('8');
    await priceDrawer.locator('.price-change').getByText('−20%',{exact:true}).waitFor();
    assert(/≈ ¥/.test(await priceDrawer.getByRole('status').innerText()),'the sample shows yuan');
    await shot('price-drawer-desktop.png');
    await page.keyboard.press('Escape');await priceDrawer.waitFor({state:'detached'});
   }
   assert.ok(await page.locator('tbody tr').count(),`${id} nonempty table`);
   assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true,`${id} desktop overflow`);
   await shot(`${id}-desktop.png`);
  }
  await nav('卡密资产');
  const cardRow=id=>page.getByRole('row').filter({has:page.getByLabel(`选择卡密 ${id}`,{exact:true})});
  assert.equal(await cardRow('fixture-card-1').getByRole('button',{name:'查看卡密',exact:true}).isDisabled(),true,'a card without kept plaintext cannot be revealed');
  await page.getByRole('textbox',{name:'搜索卡密',exact:true}).fill('fixture-card-0');
  assert.equal(await page.locator('tbody tr').count(),1);
  await page.getByRole('button',{name:'查看卡密',exact:true}).click();
  await page.getByLabel('卡密明文',{exact:true}).waitFor();
  assert.equal(await page.getByLabel('卡密明文',{exact:true}).inputValue(),'FIXTURE-RECOVERED-CODE');
  await page.getByRole('dialog',{name:'查看卡密'}).getByRole('button',{name:'关闭',exact:true}).click();
  assert.equal(await page.getByLabel('卡密明文',{exact:true}).count(),0);
  await cardRow('fixture-card-0').getByRole('button',{name:'更多操作'}).click();await page.getByRole('menuitem',{name:'冻结'}).click();await confirm();
  await cardRow('fixture-card-0').getByText('已冻结',{exact:true}).waitFor();
  await cardRow('fixture-card-0').getByRole('button',{name:'更多操作'}).click();await page.getByRole('menuitem',{name:'解冻'}).click();await confirm();
  await cardRow('fixture-card-0').getByText('使用中',{exact:true}).waitFor();
  await page.getByRole('textbox',{name:'搜索卡密',exact:true}).fill('');
  // Card details: a row click (not on a box or button) opens them: facts, history, recent calls, actions.
  await cardRow('fixture-card-0').locator('td.num').first().click();
  const cardDrawer=page.locator('#card-detail');await cardDrawer.waitFor();
  const history=cardDrawer.getByRole('region',{name:'操作记录'});await history.locator('tbody tr').first().waitFor();
  const historyRows=await history.locator('tbody tr').allInnerTexts();
  assert(historyRows[0].includes('解冻')&&historyRows[0].includes('管理员手动操作：解冻'),`newest first, with the reason: ${historyRows[0]}`);
  assert(historyRows[1].includes('冻结'));
  assert(historyRows.some(row=>row.includes('激活')&&row.includes('系统自动')),'system events are named as such');
  assert(historyRows.some(row=>row.includes('调账')&&row.includes('+50')),'adjustments show a signed amount');
  assert(historyRows.at(-1).includes('发卡')&&historyRows.at(-1).includes('2,000'),'issuing shows the credits issued');
  const recent=cardDrawer.getByRole('region',{name:'最近调用'});await recent.locator('tbody tr').first().waitFor();
  assert((await recent.locator('tbody tr').count())<=20);
  // Actions from the drawer are the list's own: they still ask first.
  const writesBeforeDrawer=fixture.writes.length;
  await cardDrawer.getByRole('button',{name:'冻结',exact:true}).click();
  const drawerConfirm=page.getByRole('alertdialog');await drawerConfirm.waitFor();assert((await drawerConfirm.innerText()).includes('冻结卡密'));
  await drawerConfirm.getByRole('button',{name:'取消',exact:true}).click();await drawerConfirm.waitFor({state:'detached'});
  assert.equal(fixture.writes.length,writesBeforeDrawer);
  await shot('card-detail-desktop.png');
  // ↓ moves to the next row in the list; ↑ comes back.
  await cardDrawer.focus();await page.keyboard.press('ArrowDown');await cardDrawer.locator('.drawer-head').getByText('fixture-card-1',{exact:true}).waitFor();
  await page.keyboard.press('ArrowUp');await cardDrawer.locator('.drawer-head').getByText('fixture-card-0',{exact:true}).waitFor();
  // A recent call opens on 调用追踪, filtered to this card, with its details open.
  await recent.locator('tbody tr').first().click();
  await page.locator('#trace-detail').waitFor();
  assert.equal(await page.getByRole('textbox',{name:'搜索调用记录',exact:true}).inputValue(),'fixture-card-0');
  await page.keyboard.press('Escape');await page.locator('#trace-detail').waitFor({state:'detached'});
  await nav('卡密资产');
  for(const tier of [1000,2000,5000,10000]){
   await page.getByRole('button',{name:'＋ 批量生成',exact:true}).click();
   await page.getByRole('combobox',{name:'积分套餐',exact:true}).selectOption(`tier-${tier}`);
   await page.getByRole('combobox',{name:'模型与计费分组',exact:true}).selectOption('fixture-group-1');
   await page.getByRole('spinbutton',{name:'生成数量',exact:true}).fill('2');
   if(tier===2000)await shot('card-creation-desktop.png');
   await page.getByRole('button',{name:'生成 2 张',exact:true}).click();await confirm();
   const results=page.getByRole('dialog',{name:'新生成的卡密'});await results.waitFor();
   assert.ok((await results.innerText()).includes(`FIXTURE-NOT-VALID-${tier}`));
   if(tier===2000){await shot('card-results-desktop.png');const download=page.waitForEvent('download');await page.getByRole('button',{name:'下载 CSV',exact:true}).click();await download;}
   await results.getByRole('button',{name:'完成',exact:true}).click();await confirm();await results.waitFor({state:'detached'});
  }
  assert.equal(fixture.writes.filter(w=>w.endpoint==='cards/batch').length,4);
  await nav('调用追踪');
  assert.equal(await page.locator('tbody tr').count(),50);
  await page.getByRole('button',{name:'下一页',exact:true}).click();
  assert.equal(await page.locator('tbody tr').count(),14);
  await page.getByRole('textbox',{name:'搜索调用记录',exact:true}).fill('不存在的请求');
  await page.getByText('没有匹配的请求',{exact:true}).waitFor();
  assert.equal(await page.getByRole('button',{name:'上一页',exact:true}).count(),0,'filtering returns to one page');
  await page.getByRole('textbox',{name:'搜索调用记录',exact:true}).fill('');
  await page.getByRole('tablist',{name:'追踪状态筛选'}).locator('[data-value="success"]').click();
  assert.equal(await page.locator('tbody tr').count(),50);
  await page.getByRole('textbox',{name:'搜索调用记录',exact:true}).fill('fixture-trace-0');
  assert.equal(await page.locator('tbody tr').count(),1);
  const requestsBefore=[];page.on('request',r=>{if(r.url().includes('/traces/content'))requestsBefore.push(r.url());});
  await page.getByRole('button',{name:'详情',exact:true}).click();
  const drawer=page.locator('#trace-detail');await drawer.waitFor();
  assert.ok((await drawer.textContent()).includes('fixture-key'));
  assert.equal(requestsBefore.length,0,'request content is read only when asked for');
  await drawer.getByRole('button',{name:'查看内容（会记录）',exact:true}).click();
  // 对话 first: this turn's question, then the model's reply; earlier turns folded.
  assert.equal(await drawer.getByRole('tab',{name:'对话'}).getAttribute('aria-selected'),'true');
  await drawer.getByText('那退出登录呢？（本地测试请求 #0）',{exact:true}).waitFor();
  await drawer.getByText('本地测试回复 #0',{exact:false}).first().waitFor();
  assert.equal(requestsBefore.length,1);assert.ok(requestsBefore[0].endsWith('invocation_id=fixture-card-0%3Afixture-inv-0'));
  await drawer.getByText('已省略 12 条较早的历史、1 张图片的内容').waitFor();
  await drawer.getByText(/^展开 \d+ 轮历史$/).waitFor();
  await page.getByRole('tab',{name:'模型回复'}).click();await drawer.getByText('本地测试回复 #0',{exact:false}).first().waitFor();
  await page.getByRole('tab',{name:'原始 JSON'}).click();assert.ok((await drawer.locator('pre').textContent()).includes('fixture-conversation-0'));
  assert.equal(requestsBefore.length,1,'one read per opened request');
  await shot('trace-detail-desktop.png');
  await page.keyboard.press('Escape');await drawer.waitFor({state:'detached'});
  assert.equal(await page.getByText('本地测试回复 #0',{exact:false}).count(),0,'closing the drawer drops the content');
  await page.getByRole('textbox',{name:'搜索调用记录',exact:true}).fill('fixture-card-2:fixture-inv-2');
  await page.getByRole('button',{name:'详情',exact:true}).first().click();await drawer.getByRole('button',{name:'查看内容（会记录）',exact:true}).click();
  await drawer.getByText('没有这次请求的内容（只保留 24 小时）',{exact:true}).waitFor();
  assert.equal(await drawer.getByText(/保留至/).count(),0,'no retention promise next to missing content');
  await page.keyboard.press('Escape');
  await page.getByRole('tablist',{name:'追踪状态筛选'}).locator('[data-value="ALL"]').click();
  await page.getByRole('textbox',{name:'搜索调用记录',exact:true}).fill('');
  await nav('财务对账');const exportFile=page.waitForEvent('download');await page.getByRole('button',{name:'导出 CSV',exact:true}).click();await exportFile;
  await page.setViewportSize({width:390,height:844});
  for(const[id,name]of pages){await nav(name);assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true,`${id} mobile overflow`);await shot(`${id}-mobile.png`);if(id==='providers'){const scrolled=await page.locator('.provider-card .table-scroll').first().evaluate(e=>{e.scrollLeft=e.scrollWidth;return e.scrollLeft>0;});assert.equal(scrolled,true,'mobile provider table actions scroll into view');assert(await page.getByRole('button',{name:'编辑',exact:true}).first().isVisible());await shot('providers-mobile-scrolled.png');}}
  await page.setViewportSize({width:1440,height:1080});
  await nav('卡密资产');
  await page.getByRole('textbox',{name:'搜索卡密',exact:true}).fill('fixture-card-0');
  fixture.expire();
  await page.getByRole('button',{name:'刷新',exact:true}).click();
  await page.getByLabel('密码',{exact:true}).waitFor();
  assert.equal(await page.getByRole('navigation').count(),0);
  assert.equal(await page.getByRole('textbox',{name:'搜索卡密',exact:true}).count(),0);
  await page.getByLabel('密码',{exact:true}).fill('fixture-password');
  await page.getByRole('button',{name:'登录',exact:true}).click();
  await page.locator('.session-clock').filter({hasText:'admin · 剩余'}).waitFor();
  await nav('卡密资产');
  assert.equal(await page.getByRole('textbox',{name:'搜索卡密',exact:true}).inputValue(),'');
  await page.getByRole('button',{name:'退出',exact:true}).click();
  await page.getByLabel('密码',{exact:true}).waitFor();
  assert.equal(await page.locator('tbody').getByText('fixture-card-0',{exact:true}).count(),0);
  assert.deepEqual(errors,[]);
  fs.writeFileSync(path.join(output,'results.json'),JSON.stringify({status:'PASS',fixtureOnly:true,pages:9,viewports:['1440x1080','390x844'],checks:['cookie login/expiry/logout', 'workspace unmounted and filters cleared on expiry', 'reveal/hide', 'unrecoverable disabled','nonempty tables','42 successes / 47 finished in 24 hours = 89.4%','24 hourly bars with 3 failures','trace drawer with on-demand content tabs','card search','freeze/unfreeze','all four issuance tiers','one-time card results','CSV downloads','trace filter/detail','no document overflow','no runtime errors'],writes:fixture.writes},null,2));
  console.log(`PASS: authenticated nonempty nine pages, chart metrics, search, freeze/unfreeze, four issuance tiers, CSV exports, trace details, desktop/mobile overflow. ${output}`);
 }finally{await browser.close();}
})().catch(e=>{console.error(e);process.exitCode=1;}).finally(()=>server.close());
