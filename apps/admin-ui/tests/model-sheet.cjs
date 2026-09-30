// 模型与定价's list, one row per customer model across groups: final build + loopback fixture
// (seeded with official prices), never production. Every publication goes through the fixture's
// copy of the server's rules.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
const recordToasts=require('./toasts.cjs');
const http=require('node:http'),fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict');
const fixture=require('./fixture-api.cjs')();
require('./pricing-seed.cjs')(fixture);
const root=path.resolve(__dirname,'../dist');
// gpt-5 is sold in PRO as well as PRO+.
const gpt5=fixture.config.models.find(model=>model.id==='fixture-model-1');
fixture.config.models.push({...gpt5,id:'fixture-model-1-pro',group_id:'fixture-group-0',sort_order:2});
const server=http.createServer(async(req,res)=>{
  try{
    if(req.url.startsWith('/api/'))return await fixture.handle(req,res);
    const file=path.resolve(root,decodeURIComponent(req.url.split('?')[0]).replace(/^\/admin\/?/,'')||'index.html');
    if(!file.startsWith(root+path.sep)||!fs.existsSync(file)){res.writeHead(404);return res.end();}
    res.setHeader('Content-Type',file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':'text/html');res.end(fs.readFileSync(file));
  }catch(error){res.writeHead(500);res.end(JSON.stringify({error:error.message}));}
});
const published=()=>fixture.writes.filter(write=>write.endpoint==='commercial-config');
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
    const dialog=page.getByRole('alertdialog');
    const toast=text=>toasts.shown(text);
    const sheet=page.getByRole('rowgroup',{name:'全部分组',exact:true});
    const row=name=>page.locator(`tr[data-model="${name}"]`);
    const shown=()=>sheet.locator('tr[data-model]').evaluateAll(rows=>rows.map(row=>row.dataset.model));
    const menu=async(name,item)=>{await button(`${name} 的更多操作`).click();const entry=page.getByRole('menuitem',{name:item,exact:true});await entry.waitFor();await entry.click();};
    // A link naming a model (运营概览's attention list) opens the list scrolled to it and marked.
    await page.goto(origin+'/admin/#/models?model=gpt-6-astra');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();
    await button('刷新').waitFor();
    await page.getByRole('navigation').getByRole('button',{name:'模型与定价',exact:true}).click();
    await page.locator('tr[data-model="gpt-6-astra"].is-marked').waitFor();
    // One row per customer model: gpt-5 once, with a chip for each group, its official price, 计费倍率,
    // what customers pay, its lowest margin, its last 7 days and what one request needs to start.
    assert.deepEqual(await shown(),['claude-sonnet','gpt-6-astra','gpt-5','gemini-pro']);
    const gptRow=await row('gpt-5').innerText();
    for(const expected of ['PRO ✓','PRO+ ✓','$4 / $20','×0.24','32 / 160','¥0.96 / ¥4.8','67%','19 次','2 张卡','1.39'])assert(gptRow.includes(expected),`${expected}\n${gptRow}`);
    assert((await row('claude-sonnet').innerText()).includes('已排期'),'the scheduled promotion shows on the row');
    assert.equal(await page.getByRole('columnheader',{name:'显示名'}).count(),0,'no 显示名 column while every row has none');
    // Filters: losses, no official price, scheduled prices.
    const filters=page.getByRole('group',{name:'筛选'});
    await filters.getByRole('button',{name:/^亏损/}).click();assert.deepEqual(await shown(),['gemini-pro']);await filters.getByRole('button',{name:/^亏损/}).click();
    await filters.getByRole('button',{name:/^未设官方价/}).click();assert.deepEqual(await shown(),['gemini-pro']);await filters.getByRole('button',{name:/^未设官方价/}).click();
    await filters.getByRole('button',{name:/^已排期/}).click();assert.deepEqual(await shown(),['claude-sonnet']);await filters.getByRole('button',{name:/^已排期/}).click();
    await filters.getByRole('button',{name:'毛利低于',exact:true}).click();await filters.getByLabel('毛利低于多少',{exact:true}).fill('70');
    assert.deepEqual((await shown()).sort(),['claude-sonnet','gemini-pro','gpt-5']);await filters.getByRole('button',{name:'毛利低于',exact:true}).click();
    // Enter opens the only match.
    await page.getByLabel('搜索模型',{exact:true}).fill('astra');await page.getByLabel('搜索模型',{exact:true}).press('Enter');
    await page.getByRole('heading',{name:/^编辑：gpt-6-astra/}).waitFor();
    await page.getByLabel('搜索模型',{exact:true}).fill('');
    console.log('PASS: one row per customer model with group chips, official price, 计费倍率, credits and ¥, lowest margin, 7-day use and start credits; filters; a named model marked; Enter opens the only match');

    // Editing a model edits every group's entry unless one is left out; the confirmation lists each
    // field changed, and a backup that cannot serve needs a tick.
    await row('gpt-5').getByRole('button',{name:'编辑',exact:true}).click();
    const scope=page.getByRole('group',{name:'修改应用到'});await scope.waitFor();
    await page.getByLabel('上下文长度',{exact:true}).fill('300000');
    await page.getByLabel('别名',{exact:true}).fill('gpt-5-latest');await page.getByLabel('别名',{exact:true}).blur();
    await button('＋ 添加备用线路').click();
    const bar=page.getByRole('region',{name:'发布'});
    await bar.getByLabel('变更原因',{exact:true}).fill('gpt-5 长上下文');await bar.getByRole('button',{name:'发布',exact:true}).click();
    await dialog.waitFor();const diff=await dialog.innerText();
    for(const expected of ['gpt-5（PRO）：上下文 200K → 300K；新增备用 OpenAI 格式 / Fixture / gpt-5（不能服务）；新增别名 gpt-5-latest','gpt-5（PRO+）：上下文 200K → 300K','这些线路现在不能服务：OpenAI 格式 / Fixture / gpt-5','基于 '])
      assert(diff.includes(expected),`${expected}\n${diff}`);
    assert(await dialog.locator('[data-confirm="accept"]').isDisabled(),'a route that cannot serve is ticked first');
    await dialog.getByRole('checkbox',{name:/我知道这些线路现在不能服务/}).check();await dialog.locator('[data-confirm="accept"]').click();
    await toast('已发布');
    const shared=published().at(-1).body.models;
    assert.deepEqual(shared.map(model=>[model.id,model.context_window,model.aliases,model.fallback_chain.map(backup=>backup.provider_id)]).sort(),
      [['fixture-model-1',300000,['gpt-5-latest'],['fixture-openai']],['fixture-model-1-pro',300000,['gpt-5-latest'],['fixture-openai']]]);
    // Left out: only the group being edited changes.
    await row('gpt-5').getByRole('button',{name:'编辑',exact:true}).click();
    await scope.getByRole('checkbox',{name:'PRO+',exact:true}).uncheck();
    await page.getByLabel('显示名称',{exact:true}).fill('GPT-5 标准');
    await bar.getByLabel('变更原因',{exact:true}).fill('PRO 的显示名');await bar.getByRole('button',{name:'发布',exact:true}).click();
    await dialog.waitFor();await dialog.locator('[data-confirm="accept"]').click();await toast('已发布');
    assert.deepEqual(published().at(-1).body.models.map(model=>[model.id,model.display_name]),[['fixture-model-1-pro','GPT-5 标准']]);
    await page.getByRole('columnheader',{name:'显示名'}).waitFor();
    console.log('PASS: an edit goes to every group\'s entry of the model unless left out; the confirmation shows field-level changes, and a backup that cannot serve is ticked first');

    // Hiding asks per group: gpt-5 is hidden in PRO+ only.
    await menu('gpt-5','隐藏（已在用的客户仍可用）');
    await dialog.waitFor();
    const hideText=await dialog.innerText();
    assert(hideText.includes('PRO · 现在：在售')&&hideText.includes('PRO+ · 现在：在售')&&hideText.includes('近 7 天：19 次请求 · 2 张卡')&&hideText.includes('最近一次使用：'),hideText);
    await dialog.getByRole('group',{name:'应用到分组'}).getByRole('checkbox',{name:'PRO',exact:true}).uncheck();
    await dialog.locator('#confirm-reason').fill('PRO+ 先下');await dialog.locator('[data-confirm="accept"]').click();
    await toast('已隐藏 gpt-5');
    assert.deepEqual(published().at(-1).body.models.map(model=>[model.id,model.visible]),[['fixture-model-1',false]]);
    await row('gpt-5').getByText('PRO+ · 隐藏').waitFor();assert.equal(await row('gpt-5').locator('td.col-status').innerText(),'部分在售');
    // Hiding with a successor: gpt-6-astra's requests go to claude-sonnet, its entry removed in the same publication.
    await menu('gpt-6-astra','隐藏（已在用的客户仍可用）');
    await dialog.waitFor();
    assert((await dialog.innerText()).includes('近 7 天：0 次请求 · 0 张卡')&&(await dialog.innerText()).includes('最近的请求里没有它'));
    await dialog.getByLabel('请求转给',{exact:true}).selectOption('claude-sonnet');
    const heir=await dialog.innerText();
    assert(heir.includes('会删除 gpt-6-astra 在 PRO 的条目，把 gpt-6-astra 加为 claude-sonnet 的别名')&&heir.includes('这些请求仍按价格表里 gpt-6-astra 的价格扣费'),heir);
    await dialog.locator('#confirm-reason').fill('换成 claude-sonnet');await dialog.locator('[data-confirm="accept"]').click();
    await toast('已隐藏 gpt-6-astra，请求转给 claude-sonnet');
    const moved=published().at(-1).body;
    assert.deepEqual(moved.removed_models,['fixture-model-3']);
    assert.deepEqual(moved.models.map(model=>[model.id,model.visible,model.aliases]),[['fixture-model-3',false,[]],['fixture-model-0',true,['gpt-6-astra']]]);
    assert(!fixture.config.models.some(model=>model.id==='fixture-model-3'),'the old entry is gone');
    assert.deepEqual(fixture.config.models.find(model=>model.id==='fixture-model-0').aliases,['gpt-6-astra']);
    await row('gpt-6-astra').waitFor({state:'detached'});
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log('PASS: hiding names each group and can leave one out; hiding with a successor removes the old entry and makes its ID the successor\'s alias in one publication, with the 7-day use and last use shown');

    // Nothing is clipped at 1280 or 1440: the status, the route and the actions stay readable.
    for(const width of [1440,1280]){
      await page.setViewportSize({width,height:900});
      const clipped=await page.locator('.sheet-table').evaluate(table=>{
        const box=table.closest('.table-scroll').getBoundingClientRect(),out=[];
        for(const cell of table.querySelectorAll('td.col-status .badge, td.col-actions button'))
          {const rect=cell.getBoundingClientRect();if(rect.width&&(rect.right>box.right+1||rect.left<box.left-1))out.push(cell.textContent||cell.getAttribute('aria-label'));}
        return {out,scroll:table.closest('.table-scroll').scrollWidth>table.closest('.table-scroll').clientWidth};
      });
      assert.deepEqual(clipped.out,[],`nothing cut off at ${width}`);assert.equal(clipped.scroll,false,`no sideways scroll at ${width}`);
      assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true,`page fits at ${width}`);
    }
    assert(await row('gpt-5').getByRole('button',{name:'调价',exact:true}).isHidden(),'below 1440 调价 sits in the ⋯ menu');
    console.log('PASS: at 1440 and 1280 the list fits without sideways scrolling; below 1440 the row keeps 编辑 and ⋯');
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
