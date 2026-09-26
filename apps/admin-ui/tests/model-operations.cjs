// 模型与定价 operations (list order and default): final build + loopback fixture, never production.
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
const published=()=>fixture.writes.filter(write=>write.endpoint==='commercial-config');
const model=id=>fixture.config.models.find(row=>row.id===id);
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
    await page.route('**/*',route=>new URL(route.request().url()).origin===origin?route.continue():route.abort());
    const button=name=>page.getByRole('button',{name,exact:true});
    const nav=name=>page.getByRole('navigation').getByRole('button',{name,exact:true}).click();
    const confirm=async({option,reason}={})=>{
      const box=page.getByRole('alertdialog');await box.waitFor();const text=await box.innerText();
      if(option!==undefined)await box.getByRole('checkbox').setChecked(option);
      if(reason!==undefined)await box.locator('#confirm-reason').fill(reason);
      await box.locator('[data-confirm="accept"]').click();await box.waitFor({state:'detached'});return text;
    };
    const bar=page.getByRole('region',{name:'发布'});
    const publish=async reason=>{await bar.getByLabel('变更原因',{exact:true}).fill(reason);await button('发布').click();await confirm();await page.locator('.toast').filter({hasText:'已发布'}).waitFor();return published().at(-1).body;};
    const group=name=>page.getByRole('rowgroup',{name,exact:true});
    const names=name=>group(name).locator('td.cell-strong').evaluateAll(cells=>cells.map(cell=>cell.firstChild.textContent));
    // gpt-6-astra shares claude-sonnet's place in PRO: which one is Kiro's default is the server's order.
    model('fixture-model-3').sort_order=0;
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();
    await button('刷新').waitFor();
    await nav('模型与定价');
    const header=group('PRO').locator('tr.group-row');
    await header.getByText('Kiro 默认：claude-sonnet 或 gpt-6-astra（谁在前以服务器为准）').waitFor();
    await header.getByText('claude-sonnet、gpt-6-astra 排在同一位置').waitFor();
    assert.equal(await group('PRO').getByText('默认待定',{exact:true}).count(),2);
    assert.deepEqual(await names('PRO'),['claude-sonnet','gpt-6-astra']);
    assert.deepEqual(await names('PRO+'),['gpt-5'],'each group lists its own models');
    // 设为默认: to the top, the group numbered again; only the entries whose place changed are sent.
    await button('gpt-6-astra 的更多操作').click();await page.getByRole('menuitem',{name:'设为默认（排到最前）',exact:true}).click();
    assert.deepEqual(await names('PRO'),['gpt-6-astra','claude-sonnet']);
    await header.getByText('Kiro 默认：gpt-6-astra').waitFor();
    assert.equal(await header.getByText('排在同一位置').count(),0,'no tie is left');
    let sent=await publish('gpt-6-astra 设为默认');
    assert.deepEqual(sent.models.map(row=>[row.id,row.sort_order]),[['fixture-model-0',1]]);
    assert.equal(model('fixture-model-0').sort_order,1);
    await group('PRO').getByRole('row').filter({hasText:'gpt-6-astra'}).getByText('默认',{exact:true}).waitFor();
    // ▲▼ move one place and renumber.
    assert(await button('上移 gpt-6-astra').isDisabled());assert(await button('下移 claude-sonnet').isDisabled());
    await button('上移 claude-sonnet').click();
    assert.deepEqual(await names('PRO'),['claude-sonnet','gpt-6-astra']);
    sent=await publish('claude-sonnet 排回第一');
    assert.deepEqual(sent.models.map(row=>[row.id,row.sort_order]).sort(),[['fixture-model-0',0],['fixture-model-3',1]]);
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log("PASS: list order: each group in Kiro's order, a tie at the top named and its default marked undecided, 设为默认 and ▲▼ renumber and publish only the moved entries");
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
