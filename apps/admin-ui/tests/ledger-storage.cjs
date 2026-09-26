// 存储与账本: the saved ledger's size, its warnings and 归档账本. Final build + loopback fixture, never production.
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
const MB=1048576;
const archives=()=>fixture.writes.filter(write=>write.endpoint==='ledger/archive');
const localDate=days=>{const date=new Date(Date.now()-days*86400000);return `${date.getFullYear()}-${String(date.getMonth()+1).padStart(2,'0')}-${String(date.getDate()).padStart(2,'0')}`;};
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
    const refresh=async()=>{await button('刷新').click();await page.locator('.btn-refresh:not([disabled])').waitFor();};
    const attention=page.locator('.attention-list');
    const badge=page.locator('#nav-badge-security');
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();await button('刷新').waitFor();
    // Below the warning level: nothing to do, nothing raised.
    await page.locator('.attention-list, .all-good').first().waitFor();
    assert.equal(await attention.getByText('账本存储',{exact:false}).count(),0);assert.equal(await badge.count(),0);
    // From the warning level (32 MB): amber; from a quarter of the ceiling (64 MB), where the server logs "archive now": red.
    fixture.storage.bytes=40*MB;await refresh();
    await attention.getByText('账本存储 40 MB / 256 MB，建议归档旧账本',{exact:false}).waitFor();
    assert.equal(await badge.getAttribute('class'),'nav-badge nav-badge-warning');
    assert.equal(await badge.locator('.sr-only').textContent(),'账本存储 40 MB / 256 MB，建议归档');
    fixture.storage.bytes=Math.round(71.2*MB);await refresh();
    const urgent=attention.locator('li').first();
    assert((await urgent.innerText()).startsWith('账本存储 71.2 MB / 256 MB，请现在归档：到上限时所有请求都会被拒绝'),'first: the most urgent');
    assert((await urgent.locator('button').getAttribute('class')).includes('is-danger'));
    assert.equal(await badge.getAttribute('class'),'nav-badge nav-badge-danger');
    await urgent.getByRole('button').click();
    await page.getByRole('heading',{name:'安全与审计',level:2,exact:true}).waitFor();
    const panel=page.getByRole('region',{name:'存储与账本'});
    await panel.getByText('已过请现在归档的 64 MB：到上限时所有请求都会被拒绝').waitFor();
    const meter=panel.getByRole('meter',{name:'账本存储'});
    assert.deepEqual([await meter.getAttribute('aria-valuenow'),await meter.getAttribute('aria-valuetext')],[String(Math.round(71.2*MB)),'71.2 MB / 256 MB']);
    console.log('PASS: the ledger size is raised amber from 32 MB and red from 64 MB on 运营概览 and the navigation, and shown with its meter on 安全与审计');

    // 归档账本: the confirmation says what moves and what stays; a date with nothing before it is refused in words.
    const date=panel.getByLabel('归档日期',{exact:true});
    assert.equal(await date.inputValue(),localDate(30),'30 days ago by default');
    await date.fill(localDate(100));await panel.getByRole('button',{name:'归档账本…',exact:true}).click();
    const box=page.getByRole('alertdialog');await box.waitFor();const facts=await box.innerText();
    for(const expected of [`归档 ${localDate(100)} 之前的账本记录？`,'账本存储现在 71.2 MB / 256 MB','之前的每次扣费、调账和充值记录移出保存的账本，写进服务器上的归档文件','卡内余额和用量额度都不变'])
      assert(facts.includes(expected),`${expected}\n${facts}`);
    await box.locator('[data-confirm="accept"]').click();await box.waitFor({state:'detached'});
    await panel.getByRole('alert').filter({hasText:'没有归档：这个日期之前没有可以归档的账本记录：换一个更晚的日期'}).waitFor();
    // Cancelled: nothing is sent.
    await date.fill(localDate(30));await panel.getByRole('button',{name:'归档账本…',exact:true}).click();
    await box.getByRole('button',{name:'取消',exact:true}).click();await box.waitFor({state:'detached'});
    assert.equal(archives().length,1);
    await panel.getByRole('button',{name:'归档账本…',exact:true}).click();await box.locator('[data-confirm="accept"]').click();await box.waitFor({state:'detached'});
    const receipt=panel.getByRole('status');await receipt.waitFor();
    const text=await receipt.innerText();
    const drained=/^已归档 ([\d,]+) 条记录（(\S+) 之前） · 账本存储 71\.2 MB → ([\d.]+) MB · 归档文件 ledger_archive_arc-\d+-0-42\.json · SHA-256 [0-9a-f]{12}…$/.exec(text);
    assert(drained&&drained[2]===localDate(30)&&Number(drained[1].replace(/,/g,''))>11000,text);
    assert.equal(`${drained[3]} MB`,`${(fixture.storage.bytes/MB).toFixed(1).replace(/\.0$/,'')} MB`,'the size after, as the server reports it');
    const sent=archives().at(-1).body;
    assert.deepEqual(sent,{beforeTsSecs:new Date(`${localDate(30)}T00:00:00`).getTime()/1000},'midnight, local time, of the day chosen');
    // After the refresh the size is under the warning level again: nothing raised.
    await page.locator('.btn-refresh:not([disabled])').waitFor();
    await badge.waitFor({state:'detached'});await panel.getByText('正常',{exact:true}).waitFor();
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log('PASS: 归档账本 confirms what moves and that balances stay, explains a refusal, sends midnight of the day chosen and shows the receipt; the warning clears');
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
