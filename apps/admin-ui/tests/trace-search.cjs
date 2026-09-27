// 调用追踪 searched on the server: a time range (近 1 小时, 近 24 小时 or chosen minutes), card, model,
// provider and status among every request kept, not only the latest 500; totals of what is shown
// (次数 / 失败 / 扣费 / 成本); under 失败 the failures by model and by provider, by name; and an older
// server, which ignores the filters and sends no totals, narrowed here and said so. Final build +
// loopback fixture, never production.
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
// 600 older requests, two to four days back, one every five minutes: the latest 500 no longer hold
// them all. Every 30th failed on the OpenAI-format provider before anything was answered.
for(let i=0;i<600;i++){
  const failed=i%30===0,card=fixture.cards[i%6];
  fixture.traces.push({id:`old-trace-${i}`,card_id:card.id,ts:now-2*86400-i*300,invocation_id:`${card.id}:old-${i}`,exposed_model:failed?'gpt-5':'claude-sonnet',status:failed?'error':'success',
    ttft_ms:failed?null:500,tokens_per_second:failed?null:40,error_class:failed?'upstream_start_failed':null,provider_id:failed?null:'fixture-provider',
    input_tokens:failed?0:1000,output_tokens:failed?0:100,credits_charged:failed?0:1000000,provider_cost_micro_cny:failed?0:10000,
    attempt_chain:failed?[{provider_id:'fixture-openai',key_id:'fixture-openai-key-1',success:false,error:'http_529',latency_ms:800}]:[{provider_id:'fixture-provider',key_id:'fixture-key',success:true,error:null,latency_ms:600}]});
}
const pad=value=>String(value).padStart(2,'0');
const minute=secs=>{const d=new Date(secs*1000);return `${d.getFullYear()}-${pad(d.getMonth()+1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;};
const minuteStart=secs=>{const d=new Date(secs*1000);d.setSeconds(0,0);return Math.floor(d.getTime()/1000);};
// As the page shows them: charges to four places, money as formatMoney.
const charge=micro=>(micro/1e6).toLocaleString('en-US',{maximumFractionDigits:4});
const money=micro=>{const digits=micro&&Math.abs(micro)<10000?4:2;return `${micro<0?'-':''}¥${Math.abs(micro/1e6).toLocaleString('en-US',{minimumFractionDigits:digits,maximumFractionDigits:digits})}`;};
const totalsOf=list=>`次数 ${list.length.toLocaleString('en-US')} 失败 ${list.filter(t=>t.status==='error').length} 扣费 ${charge(list.reduce((sum,t)=>sum+t.credits_charged,0))} 积分 成本 ${money(list.reduce((sum,t)=>sum+t.provider_cost_micro_cny,0))}`;
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
    const searches=[];page.on('request',request=>{const url=new URL(request.url());if(url.pathname==='/api/v1/admin/traces')searches.push(Object.fromEntries(url.searchParams));});
    await page.route('**/*',route=>new URL(route.request().url()).origin===origin?route.continue():route.abort());
    const button=name=>page.getByRole('button',{name,exact:true});
    const nav=name=>page.getByRole('navigation').getByRole('button',{name,exact:true}).click();
    const tab=value=>page.getByRole('tablist',{name:'追踪状态筛选'}).locator(`[data-value="${value}"]`);
    const chip=name=>page.getByRole('group',{name:'时间范围'}).getByRole('button',{name,exact:true});
    const summary=async()=>(await page.locator('.filter-summary').innerText()).replace(/\s+/g,' ').trim();
    const footer=async()=>(await page.getByRole('group',{name:'合计',exact:true}).innerText()).replace(/\s+/g,' ').trim();
    const rows=page.locator('.traces-table tbody tr:not(.state-row)');
    const chipTexts=async group=>(await page.getByRole('group',{name:group}).getByRole('button').allInnerTexts()).map(text=>text.replace(/\s+/g,' '));
    const waitRows=count=>page.waitForFunction(count=>document.querySelectorAll('.traces-table tbody tr:not(.state-row)').length===count,count);
    const waitText=async(read,expected)=>{for(let i=0;i<100;i++){const value=await read();if(value===expected||(expected instanceof RegExp&&expected.test(value)))return value;await page.waitForTimeout(50);}assert.fail(`expected ${expected}, got ${await read()}`);};
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();await button('刷新').waitFor();
    await nav('调用追踪');await rows.first().waitFor();

    // The latest 500, and the totals of all 664 kept.
    const all=fixture.traces;
    assert.match(await summary(),/^最近 500 次请求（\d\d-\d\d \d\d:\d\d 起），共保留 664 次$/);
    assert.equal(await footer(),`合计 ${totalsOf(all)}`);
    assert.deepEqual([await tab('ALL').locator('.tab-count').innerText(),await tab('error').locator('.tab-count').innerText()],['664','23']);
    assert.equal(await tab('success').locator('.tab-count').count(),0,'no count the latest 500 cannot give');
    console.log('PASS: the latest 500 are listed with the totals over all 664 kept requests, 全部 and 失败 counted by the server');

    // 失败: the server's own failures, all 23 of them, by model and by provider (by name). They are asked for
    // as soon as more matched than were returned, so that 失败 and 拒绝 are counted apart.
    await tab('error').click();await waitRows(23);
    assert(searches.some(search=>search.status==='error'),JSON.stringify(searches));
    assert.equal(await summary(),'服务器按筛选找到 23 次 · 匹配 23 条 清除筛选');
    assert.equal(await footer(),`筛选合计 ${totalsOf(all.filter(t=>t.status==='error'))}`);
    assert.deepEqual(await chipTexts('失败按模型'),['gpt-5 21','claude-sonnet 1','gemini-pro 1']);
    const byProvider=page.getByRole('group',{name:'失败按供应商'});
    assert.deepEqual(await chipTexts('失败按供应商'),['OpenAI 格式 / Fixture 20','测试供应商 / Fixture 3']);
    // A provider's chip narrows to the requests sent to it, the one it failed first included.
    searches.length=0;await byProvider.getByRole('button',{name:/^OpenAI 格式 \/ Fixture/}).click();
    await waitText(summary,'服务器按筛选找到 20 次 · 匹配 20 条 清除筛选');
    assert.deepEqual(searches.at(-1),{limit:'500',provider:'fixture-openai'});
    assert.equal(await page.getByLabel('供应商筛选',{exact:true}).inputValue(),'fixture-openai');
    assert.equal(await page.evaluate(()=>location.hash),'#/traces?status=error&provider=fixture-openai');
    await byProvider.getByRole('button',{name:/^OpenAI 格式 \/ Fixture/}).click();await waitRows(23);
    // A model's chip does the same for the model.
    await page.getByRole('group',{name:'失败按模型'}).getByRole('button',{name:/^claude-sonnet/}).click();await waitRows(1);
    assert.equal(await page.getByLabel('模型筛选',{exact:true}).inputValue(),'claude-sonnet');
    await page.getByLabel('模型筛选',{exact:true}).selectOption('ALL');await waitRows(23);
    console.log('PASS: 失败 lists every failure the server keeps, with totals, broken down by model and by provider name; a chip narrows to it');

    // 近 1 小时 asks from an hour ago.
    await tab('ALL').click();searches.length=0;await chip('近 1 小时').click();
    await waitText(summary,/^服务器按筛选找到 \d+ 次 · 匹配 \d+ 条 清除筛选$/);
    const hour=searches.at(-1);assert(Math.abs(Number(hour.fromSecs)-(Math.floor(Date.now()/1000)-3600))<30&&!hour.toSecs,JSON.stringify(hour));
    // 自定义: chosen minutes, the last included, read from the server; the address keeps them.
    await chip('自定义').click();
    const start=now-2*86400-6*3600,end=now-2*86400-3600,from=minuteStart(start),to=minuteStart(end)+60;
    const inRange=all.filter(t=>t.ts>=from&&t.ts<to);
    await page.getByLabel('开始时间',{exact:true}).fill(minute(start));await page.getByLabel('结束时间',{exact:true}).fill(minute(end));
    const words=text=>text.slice(5).replace('T',' ');
    await waitText(summary,`服务器按筛选找到 ${inRange.length} 次（${words(minute(start))} 至 ${words(minute(end))}） · 匹配 ${inRange.length} 条 清除筛选`);
    assert.deepEqual(searches.at(-1),{limit:'500',fromSecs:String(from),toSecs:String(to)});
    assert.equal(await footer(),`筛选合计 ${totalsOf(inRange)}`);
    const address=`#/traces?range=custom&from=${encodeURIComponent(minute(start))}&to=${encodeURIComponent(minute(end))}`;
    assert.equal(await page.evaluate(()=>location.hash),address);
    await page.reload();await rows.first().waitFor();
    assert.equal(await page.getByLabel('开始时间',{exact:true}).inputValue(),minute(start));
    await waitText(footer,`筛选合计 ${totalsOf(inRange)}`);
    // The end before the start is no range: nothing is asked.
    await page.getByLabel('结束时间',{exact:true}).fill(minute(start-600));
    await waitText(summary,'时间范围不对：结束要晚于开始 · 匹配 0 条');
    const asked=searches.length;await page.waitForTimeout(400);assert.equal(searches.length,asked,'nothing is asked for a reversed range');
    await page.locator('.list-state').getByText('时间范围不对：结束要晚于开始',{exact:true}).waitFor();
    console.log('PASS: 近 1 小时 and chosen minutes are asked of the server (the last minute included), kept in the address across a reload; a reversed range asks nothing');

    // The search box narrows what was read: the totals are of those, and say so when more matched.
    await chip('全部').click();await rows.first().waitFor();
    await page.getByLabel('搜索调用记录',{exact:true}).fill('gemini');
    const gemini=all.slice(0,500).filter(t=>t.exposed_model==='gemini-pro');
    await waitText(footer,`筛选合计 ${totalsOf(gemini)} 只算已读取的 500 次`);
    await page.getByLabel('搜索调用记录',{exact:true}).fill('');
    console.log('PASS: the search box narrows the requests read, and their totals say they cover only those');

    // An older server reads only the card and sends no totals: the latest 500 are narrowed here, and said to be.
    await page.route('**/api/v1/admin/traces?*',async route=>{
      const url=new URL(route.request().url()),limit=url.searchParams.get('limit'),card=url.searchParams.get('card_id');
      const response=await route.fetch({url:`${origin}/api/v1/admin/traces?limit=${limit}${card?`&card_id=${card}`:''}`});const body=await response.json();delete body.totals;delete body.count;await route.fulfill({json:body});
    });
    await button('刷新').click();await page.locator('.btn-refresh:not([disabled])').waitFor();
    await waitText(summary,/^最近 500 次请求（\d\d-\d\d \d\d:\d\d 起）$/);
    assert.equal(await tab('success').locator('.tab-count').count(),1,'counted from what was read');
    await page.getByLabel('模型筛选',{exact:true}).selectOption('gpt-5');
    const gpt=all.slice(0,500).filter(t=>t.exposed_model==='gpt-5');
    await waitText(summary,new RegExp(`^最近 500 次请求（\\d\\d-\\d\\d \\d\\d:\\d\\d 起） · 匹配 ${gpt.length} 条 清除筛选$`));
    await waitText(footer,`筛选合计 ${totalsOf(gpt)} 只算已读取的 500 次`);
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log('PASS: an older server\'s latest 500 are narrowed by the same rules, the totals marked as covering only those');
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
