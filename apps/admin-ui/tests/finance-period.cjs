// 财务对账 by period: 今天 / 昨天 / 本月 / 上月 / 累计 / 自定义 read from the server by fromSecs and toSecs; 按供应商
// to check against each upstream's bill; the margin over costed requests with the rest named; sales, the
// balances still owed, negative margins in red and raised on 运营概览, which shows 今日收入 / 成本 / 毛利; and
// the ledger CSV of the period. Final build + loopback fixture, never production.
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
// Local calendar bounds, as the console computes them.
const secs=date=>Math.floor(date.getTime()/1000),today=new Date(),[y,m,d]=[today.getFullYear(),today.getMonth(),today.getDate()];
const bounds={today:[new Date(y,m,d),new Date(y,m,d+1)],yesterday:[new Date(y,m,d-1),new Date(y,m,d)],month:[new Date(y,m,1),new Date(y,m+1,1)],lastMonth:[new Date(y,m-1,1),new Date(y,m,1)]};
const dateText=date=>`${date.getFullYear()}-${String(date.getMonth()+1).padStart(2,'0')}-${String(date.getDate()).padStart(2,'0')}`;
// formatMoney: four decimals under a fen, the sign before ¥.
const money=micro=>{const digits=micro&&Math.abs(micro)<10000?4:2;return `${micro<0?'-':''}¥${Math.abs(micro/1e6).toLocaleString('en-US',{minimumFractionDigits:digits,maximumFractionDigits:digits})}`;};
// periodText: one day by its date, several as first 至 last.
const rangeText=(start,end)=>{const first=dateText(new Date(start*1000)),last=dateText(new Date((end-1)*1000));return first===last?first:`${first} 至 ${last}`;};
// What the fixture's ledger adds up to over [from, to), as the page should show it.
const sums=(from,to)=>{
  const entries=fixture.ledger.filter(entry=>(from===undefined||entry.ts>=from)&&(to===undefined||entry.ts<to)),face=fixture.config.settings.credit_face_value_cny;
  const costed=entries.filter(entry=>entry.rate_card_version),revenue=list=>list.reduce((sum,entry)=>sum+Math.round(entry.credits_charged*face),0),cost=list=>list.reduce((sum,entry)=>sum+entry.provider_cost_micro_cny,0);
  return {count:entries.length,revenue:revenue(entries),cost:cost(entries),gross:revenue(costed)-cost(costed),rate:revenue(costed)?(revenue(costed)-cost(costed))/revenue(costed)*100:null,uncosted:entries.length-costed.length,entries};
};
(async()=>{
  let browser;
  try{
    await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
    browser=await chromium.launch({headless:true,...(process.env.CHROME_PATH?{executablePath:process.env.CHROME_PATH}:{})});
    const page=await browser.newPage({viewport:{width:1440,height:1000},acceptDownloads:true}),errors=[];
    page.setDefaultTimeout(10000);
    const origin=`http://127.0.0.1:${server.address().port}`;
    page.on('pageerror',error=>{errors.push(error.message);console.error('Browser error:',error.message);});
    const nativeDialogs=[];page.on('dialog',dialog=>{nativeDialogs.push(dialog.message());void dialog.dismiss();});
    const periods=[];page.on('request',request=>{const url=new URL(request.url());if(url.pathname==='/api/v1/admin/financials')periods.push(Object.fromEntries(url.searchParams));});
    await page.route('**/*',route=>new URL(route.request().url()).origin===origin?route.continue():route.abort());
    const button=name=>page.getByRole('button',{name,exact:true});
    const nav=name=>page.getByRole('navigation').getByRole('button',{name,exact:true}).click();
    const kpi=label=>page.locator('.kpi').filter({has:page.locator('.kpi-label',{hasText:label})});
    const tab=value=>page.getByRole('tablist',{name:'统计区间'}).locator(`[data-value="${value}"]`).click();
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();await button('刷新').waitFor();

    // 运营概览: today's revenue, cost and margin, read with today's bounds; a model sold below cost today is raised.
    const [from,to]=bounds.today.map(secs),day=sums(from,to);
    const strip=page.getByRole('group',{name:'今日收入'});await strip.getByText(money(day.revenue),{exact:true}).waitFor();
    assert(periods.some(query=>query.fromSecs===String(from)&&query.toSecs===String(to)),JSON.stringify(periods));
    const figures=await strip.locator('b').allInnerTexts();
    assert.deepEqual(figures,[money(day.revenue),money(day.cost),money(day.gross)],figures.join(' | '));
    if(day.uncosted)await strip.getByText(`另有 ${day.uncosted} 次未设成本`,{exact:false}).waitFor();
    if(day.entries.some(entry=>entry.exposed_model==='gpt-6-astra')){
      // One model: to its price on 模型与定价, or to 财务对账.
      const loss=page.locator('.attention-list li').filter({has:page.getByText(/^今天 gpt-6-astra 毛利 -\d+\.\d%：售价低于采购价，按成本在亏$/)});
      await loss.getByRole('button',{name:'查看模型',exact:true}).click();
      await page.locator('tr[data-model="gpt-6-astra"].is-marked').waitFor();assert.equal(await page.evaluate(()=>location.hash),'#/models?model=gpt-6-astra');
      await nav('运营概览');
      await loss.getByRole('button',{name:'去财务对账',exact:true}).click();await page.getByRole('heading',{name:'财务对账',level:2,exact:true}).waitFor();
    } else {console.log('(the loss-making requests fell before midnight: the attention item is not checked)');await nav('财务对账');}
    console.log('PASS: 运营概览 shows 今日收入 / 成本 / 毛利 from today\'s financials and raises a model sold below cost today');

    // 累计 (the kept ledger) by default: the margin over costed requests, the one without a cost named.
    const all=sums();
    await kpi('毛利').getByText(`另有 ${all.uncosted} 次未设成本`,{exact:true}).waitFor();
    assert.equal(await kpi('毛利').locator('.kpi-value').innerText(),money(all.gross));
    assert.equal(await kpi('消耗积分').locator('.kpi-value').innerText(),'25');
    await page.getByText('统计区间：累计（全部保留账本）',{exact:true}).waitFor();
    // 按供应商: requests, the four kinds of tokens and the cost, with a total, to set against each upstream's bill.
    const providers=page.getByRole('region',{name:'按供应商'});
    assert.deepEqual(await providers.getByRole('row').filter({hasText:'OpenAI 格式 / Fixture'}).locator('td').allInnerTexts(),['OpenAI 格式 / Fixture','2','23,000','1,870','60,000','4,000','¥0.07']);
    assert.deepEqual(await providers.locator('tfoot td').allInnerTexts(),['合计','10','125,000','9,450','300,000','20,000','¥0.12']);
    // A model sold below its cost is red.
    const astra=page.getByRole('region',{name:'按模型'}).getByRole('row').filter({hasText:'gpt-6-astra'}).locator('td').last();
    assert.deepEqual([await astra.innerText(),await astra.getAttribute('class')],['-32.0%','num is-danger']);
    // Sales at the plan's price, and what customers can still use, now.
    // The line wraps as a row of words: read it as one line.
    const words=async name=>(await page.getByRole('region',{name}).innerText()).replace(/\s+/g,' ');
    const sales=await words('销售');
    assert(sales.includes('发卡 6 张 · ¥330.00 · 激活 6 张 · ¥330.00')&&sales.includes('PRO+'),sales);
    const owed=await words('未消耗余额');
    assert(owed.includes('7,200 积分 ≈ ¥72.00')&&owed.includes('4 张卡，其中未激活 1 张 · 1,900 积分')&&owed.includes('现在，不随统计区间变化'),owed);
    console.log('PASS: 累计 shows the margin over costed requests with 另有 1 次未设成本, 按供应商 with totals, a loss in red, sales at plan price and the balances still owed');

    // Each period is read from the server with its local bounds.
    for(const [value,label] of [['today','今天'],['yesterday','昨天'],['month','本月'],['lastMonth','上月']]){
      await tab(value);
      const [start,end]=bounds[value].map(secs),expected=sums(start,end);
      await page.getByText(`统计区间：${rangeText(start,end)}`,{exact:true}).waitFor();
      // Figures show only once the period's own answer is in (a period's figures are cleared when it changes).
      await kpi('消耗积分').locator('.kpi-sub').getByText(`${expected.count} 次结算`,{exact:true}).waitFor();
      assert.deepEqual(periods.at(-1),{fromSecs:String(start),toSecs:String(end)},`${label}: ${JSON.stringify(periods)}`);
      assert.equal(await kpi('毛利').locator('.kpi-value').innerText(),money(expected.gross),label);
    }
    // 自定义: both days included; the last before the first is no period, and nothing is asked.
    await tab('custom');const first=page.getByLabel('开始日期',{exact:true}),last=page.getByLabel('结束日期',{exact:true});
    await page.getByText(`统计区间：${rangeText(secs(new Date(y,m,1)),secs(new Date(y,m,d+1)))}`,{exact:true}).waitFor();
    const tenDays=new Date(y,m,d-10),yesterday=new Date(y,m,d-1);
    await first.fill(dateText(yesterday));await last.fill(dateText(tenDays));
    await page.getByText('请选择开始和结束日期（结束不早于开始）',{exact:true}).waitFor();
    const asked=periods.length;await page.waitForTimeout(400);assert.equal(periods.length,asked,'nothing is asked for a reversed choice');
    assert.equal(await kpi('毛利').locator('.kpi-value').innerText(),'—');
    assert(await button('导出 CSV').isDisabled());
    await first.fill(dateText(tenDays));await last.fill(dateText(yesterday));
    const chosen=[secs(tenDays),secs(new Date(y,m,d))];
    await page.getByText(`统计区间：${dateText(tenDays)} 至 ${dateText(yesterday)}`,{exact:true}).waitFor();
    await kpi('消耗积分').locator('.kpi-sub').getByText(`${sums(...chosen).count} 次结算`,{exact:true}).waitFor();
    assert.deepEqual(periods.at(-1),{fromSecs:String(chosen[0]),toSecs:String(chosen[1])});
    console.log('PASS: 今天, 昨天, 本月, 上月 and 自定义 are read with their local bounds, from inclusive and to exclusive; a reversed choice asks nothing');

    // The CSV of a period: the server's ledger cut to its entries, each kept as written, readable in a spreadsheet.
    await tab('today');await kpi('消耗积分').locator('.kpi-sub').getByText(`${day.count} 次结算`,{exact:true}).waitFor();
    let download=page.waitForEvent('download');await button('导出 CSV').click();
    let file=await download;assert.equal(file.suggestedFilename(),`ledger-${dateText(today)}.csv`);
    const cut=fs.readFileSync(await file.path(),'utf8');
    assert(cut.startsWith('\uFEFFid,card_id,ts,kind,'),cut.slice(0,40));
    const lines=cut.trim().split('\n');assert.equal(lines.length,1+day.count);
    assert(lines.slice(1).every(line=>{const ts=Number(line.split(',')[2]);return ts>=from&&ts<to;}),'only today\'s entries');
    // 累计, or 导出全部账本 CSV: the whole file as the server wrote it.
    download=page.waitForEvent('download');await page.getByRole('button',{name:'更多导出'}).click();await page.getByRole('menuitem',{name:'导出全部账本 CSV'}).click();
    file=await download;assert(file.suggestedFilename().startsWith('ledger-export-'));
    assert.equal(fs.readFileSync(await file.path(),'utf8').trim().split('\n').length,1+fixture.ledger.length);
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log('PASS: 导出 CSV cuts the ledger to the period (header kept, BOM for spreadsheets); 导出全部账本 CSV is the server\'s whole file');
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
