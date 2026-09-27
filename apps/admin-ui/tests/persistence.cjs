// 存储与账本: whether the server saved its latest change and when it last saved. A failed save, which
// makes the server refuse every change and every request, is said in words with its fix, first in
// 需要关注 and on 安全与审计's badge; an older server that reports neither shows nothing. Final
// build + loopback fixture, never production.
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
const pad=value=>String(value).padStart(2,'0');
const clock=secs=>{const date=new Date(secs*1000);return `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;};
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
    const panel=page.getByRole('region',{name:'存储与账本'});
    const words=async locator=>(await locator.innerText()).replace(/\s+/g,' ').trim();
    // Midnight must not fall between saving and reading (the time would carry a date).
    fixture.storage.savedAt=Math.max(Math.floor(Date.now()/1000)-180,Math.floor(new Date().setHours(0,0,0,0)/1000));
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();await button('刷新').waitFor();

    // Saved: when, on the panel; nothing raised.
    await nav('安全与审计');
    const ago=Math.round((Date.now()/1000-fixture.storage.savedAt)/60);
    assert.match(await words(panel.locator('.storage-saved')),new RegExp(`^已保存 最近保存 ${clock(fixture.storage.savedAt)}（(?:(?:${ago}|${ago-1}|${ago+1}) 分钟前|刚刚)）$`));
    assert.equal(await panel.getByRole('alert').count(),0);
    assert.equal(await page.locator('#nav-badge-security').count(),0);
    console.log('PASS: a saved state shows 已保存 and when it last saved, and raises nothing');

    // The disk is full: the server refuses every change and every request until a save succeeds.
    fixture.storage.persistenceError='The disk holding the saved state is full';
    await refresh();
    const alert=panel.getByRole('alert');await alert.waitFor();
    const said=await words(alert);
    assert(said.startsWith('保存失败：保存数据的磁盘满了 在下一次保存成功之前，服务器拒绝所有修改和客户的每个请求。请清理服务器上保存数据的磁盘。'),said);
    assert(said.includes(`最近一次保存成功：${clock(fixture.storage.savedAt)}（`),said);
    assert.equal(await panel.locator('.storage-saved').count(),0);
    const badge=page.locator('#nav-badge-security');
    assert.deepEqual([await badge.getAttribute('title'),await badge.getAttribute('class')],['保存失败：保存数据的磁盘满了','nav-badge nav-badge-danger']);
    // The size ceiling: the fix is the archive right below.
    fixture.storage.persistenceError='The saved state has reached its size ceiling; archive old ledger entries';
    await refresh();
    await panel.getByText('请现在归档旧账本（下方“归档账本…”）。',{exact:false}).waitFor();
    assert(await button('归档账本…').isEnabled());
    // First in 需要关注, leading here.
    await nav('运营概览');
    const first=page.locator('.attention-list li').first().getByRole('button',{name:'保存失败：保存的数据到了大小上限，服务器现在拒绝所有修改和客户请求',exact:true});
    assert((await first.getAttribute('class')).includes('is-danger'));
    await first.click();await page.getByRole('heading',{name:'安全与审计',level:2,exact:true}).waitFor();
    // Saved again: the alert goes.
    fixture.storage.persistenceError=null;fixture.storage.savedAt=Math.floor(Date.now()/1000);
    await refresh();await panel.locator('.storage-saved').getByText('已保存',{exact:true}).waitFor();
    assert.equal(await panel.getByRole('alert').count(),0);assert.equal(await badge.count(),0);
    console.log('PASS: a failed save is said in words with its fix and the last good save, first in 需要关注 and on 安全与审计\'s badge, and goes once saving works');

    // An older server reports neither: nothing is shown or raised.
    await page.route('**/api/v1/admin/stats',async route=>{const response=await route.fetch();const body=await response.json();
      for(const field of ['lastSavedAtSecs','persistenceReady','persistenceError'])delete body[field];await route.fulfill({json:body});});
    await refresh();
    assert.equal(await panel.locator('.storage-saved').count(),0);assert.equal(await panel.getByRole('alert').count(),0);
    await panel.locator('.storage-line').waitFor();
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log('PASS: an older server that reports neither shows the storage size as before, with no save status');
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
