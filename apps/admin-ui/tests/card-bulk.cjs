// Final-build auth gate regression: localhost fixture only, no deployed services.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
const http=require('node:http'),fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict');
const fixture=require('./fixture-api.cjs')();
async function waitForRoute(ready){
  const deadline=Date.now()+10000;
  while(!ready()){assert(Date.now()<deadline,'timed out waiting for intercepted request');await new Promise(r=>setTimeout(r,10));}
}
const root=path.resolve(__dirname,'../dist');

const server=http.createServer(async(req,res)=>{
  try{
    if(req.url.startsWith('/api/'))return await fixture.handle(req,res);
    const file=path.resolve(root,decodeURIComponent(req.url.split('?')[0]).replace(/^\/admin\/?/,'')||'index.html');
    if(!file.startsWith(root+path.sep)||!fs.existsSync(file)){res.writeHead(404);return res.end();}
    res.setHeader('Content-Type',file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':'text/html');res.end(fs.readFileSync(file));
  }catch(error){res.writeHead(500);res.end(JSON.stringify({error:error.message}));}
});

(async()=>{
  await new Promise(r=>server.listen(0,'127.0.0.1',r));
  const browser=await chromium.launch({headless:true,...(process.env.CHROME_PATH?{executablePath:process.env.CHROME_PATH}:{})});
  try {
    const page=await browser.newPage({acceptDownloads:true});
    const origin=`http://127.0.0.1:${server.address().port}`;
    const cards=Array.from({length:55},(_,i)=>({id:`bulk-${i}`,codeRecoverable:i!==2,status:'active',groupId:'fixture',pointsTotal:10,pointsAvailable:10,boundDevices:[],maxDevices:1}));
    let statusCalls=[],revealCalls=[],held;
    await page.route('**/*',route=>new URL(route.request().url()).origin===origin?route.continue():route.abort());
    await page.route('**/api/v1/admin/cards?*',route=>route.fulfill({json:{success:true,cards,count:cards.length}}));
    await page.route('**/api/v1/admin/cards/status',async route=>{
      const body=route.request().postDataJSON(); statusCalls.push(body);
      if(body.cardId==='bulk-0' && statusCalls.length===1){held=route;return;}
      if(body.action==='void' && body.cardId!=='bulk-1')cards.find(c=>c.id===body.cardId).status='voided';
      if(body.action==='archive')cards.find(c=>c.id===body.cardId).archivedAt=1773100000;
      if(body.action==='unarchive')cards.find(c=>c.id===body.cardId).archivedAt=null;
      await route.fulfill({json:{success:body.cardId!=='bulk-1'}});
    });
    await page.route('**/api/v1/admin/cards/reveal',route=>{
      const {cardId}=route.request().postDataJSON();revealCalls.push(cardId);
      return route.fulfill({json:cardId==='bulk-1'?{success:false}:{success:true,rawCode:`TEST-ONLY-${cardId}`}});
    });
    await page.goto(origin+'/admin/');
    await page.getByLabel('密码',{exact:true}).fill('fixture-password');
    await page.getByRole('button',{name:'登录',exact:true}).click();
    await page.getByRole('navigation').getByRole('button',{name:'卡密资产',exact:true}).click();
    const button=name=>page.getByRole('button',{name,exact:true});
    const check=id=>page.getByLabel(`选择卡密 bulk-${id}`,{exact:true});
    await check(0).waitFor();
    assert(await button('批量冻结').isDisabled());
    assert.equal(await page.getByRole('checkbox').count(),50);
    await button('当前页全选').click();assert.equal(await page.getByRole('checkbox',{checked:true}).count(),50);
    await button('下一页').click();assert.equal(await page.getByRole('checkbox',{checked:true}).count(),0);
    assert.equal(await page.getByRole('checkbox').count(),5);
    await button('当前页全选').click();await button('清空选择').click();assert(await button('批量封禁').isDisabled());
    await button('上一页').click();await check(0).check();
    await page.getByLabel('搜索卡密',{exact:true}).fill('bulk-1');assert.equal(await page.getByRole('checkbox',{checked:true}).count(),0);
    await page.getByLabel('搜索卡密',{exact:true}).fill('');await check(0).check();await check(1).check();
    page.once('dialog',d=>d.dismiss());await button('批量冻结').click();assert.equal(statusCalls.length,0);
    page.once('dialog',d=>{assert(d.message().includes('2 张'));return d.accept();});await button('批量冻结').click();
    await waitForRoute(()=>held);assert(await button('批量冻结').isDisabled());assert(await check(0).isDisabled());
    assert(await page.getByLabel('搜索卡密',{exact:true}).isDisabled());
    await button('批量冻结').evaluate(el=>el.click());assert.equal(statusCalls.length,1);
    await held.fulfill({json:{success:true}});
    const results=page.getByLabel('批量操作结果',{exact:true});
    await results.getByText('bulk-1：失败或结果未确认，请刷新核对后再操作',{exact:true}).waitFor();
    await page.waitForFunction(()=>!document.querySelector('input[type=checkbox]').disabled);
    assert.deepEqual(statusCalls.map(c=>c.cardId),['bulk-0','bulk-1']);assert.equal(await page.getByRole('checkbox',{checked:true}).count(),1);assert(await check(1).isChecked());await button('清空选择').click();
    for(const [name,action] of [['批量解冻','unfreeze'],['批量封禁','ban']]){
      cards[3].status=action==='unfreeze'?'frozen':'active';await button('刷新').click();await page.waitForFunction(()=>!document.querySelector('input[type=checkbox]').disabled);
      await check(3).check();page.once('dialog',d=>d.accept());await button(name).click();
      await page.waitForFunction(()=>!document.querySelector('input[type=checkbox]').disabled);
      assert.equal(statusCalls.at(-1).action,action);assert.equal(statusCalls.at(-1).cardId,'bulk-3');
    }
    await check(0).check();await check(1).check();await check(2).check();
    page.once('dialog',d=>d.accept());const downloadPromise=page.waitForEvent('download');await button('导出已选卡密').click();
    const download=await downloadPromise;
    assert.equal(fs.readFileSync(await download.path(),'utf8'),'TEST-ONLY-bulk-0');
    await page.waitForFunction(()=>!document.querySelector('input[type=checkbox]').disabled);
    assert.deepEqual(revealCalls,['bulk-0','bulk-1']);
    assert((await results.innerText()).includes('历史卡密不可恢复'));
    assert.equal(await page.getByRole('checkbox',{checked:true}).count(),2);assert(await check(1).isChecked());assert(await check(2).isChecked());
    assert.equal(await page.evaluate(()=>JSON.stringify({...localStorage,...sessionStorage}).includes('TEST-ONLY-')),false);
    assert.equal((await page.locator('body').innerText()).includes('TEST-ONLY-'),false);
    await check(0).check();await button('刷新').click();await page.waitForFunction(()=>!document.querySelector('input[type=checkbox]').disabled);
    assert.equal(await page.getByRole('checkbox',{checked:true}).count(),0);
    // Ineligible states must never be sent; failed selections must not escape a new filter.
    cards[0].status='banned';await button('刷新').click();await page.waitForFunction(()=>!document.querySelector('input[type=checkbox]').disabled);
    await check(0).check();const before=statusCalls.length;page.once('dialog',d=>d.accept());await button('批量冻结').click();
    await page.waitForFunction(()=>!document.querySelector('input[type=checkbox]').disabled);
    assert.equal(statusCalls.length,before);assert(await check(0).isChecked());
    await page.getByLabel('搜索卡密',{exact:true}).fill('bulk-4');assert.equal(await page.getByRole('checkbox',{checked:true}).count(),0);
    await check(4).check();page.once('dialog',d=>d.accept());await button('批量封禁').click();await page.waitForFunction(()=>!document.querySelector('input[type=checkbox]').disabled);
    assert.equal(statusCalls.at(-1).cardId,'bulk-4');assert.equal(statusCalls.length,before+1);
    await page.getByLabel('搜索卡密',{exact:true}).fill('');
    // Delete means irreversible void, and must skip issued/used/voided cards.
    cards[0].status='unactivated';cards[1].status='active';cards[2].status='voided';
    await page.getByLabel('状态筛选',{exact:true}).selectOption('ALL');
    await button('刷新').click();await page.waitForFunction(()=>!document.querySelector('input[type=checkbox]').disabled);
    assert(await button('批量删除（未激活）').isDisabled());
    await check(0).check();await check(1).check();await check(2).check();
    const beforeVoid=statusCalls.length;
    page.once('dialog',d=>d.dismiss());await button('批量删除（未激活）').click();assert.equal(statusCalls.length,beforeVoid);
    page.once('dialog',d=>{assert(d.message().includes('3 张'));assert(d.message().includes('不可恢复'));assert(d.message().includes('财务与审计记录保留'));return d.accept();});
    await button('批量删除（未激活）').click();await page.waitForFunction(()=>!document.querySelector('input[type=checkbox]').disabled);
    assert.equal(statusCalls.length,beforeVoid+1);assert.equal(statusCalls.at(-1).action,'void');assert.equal(statusCalls.at(-1).cardId,'bulk-0');
    assert((await results.innerText()).includes('删除（永久作废）成功'));
    assert.equal(await page.getByRole('checkbox',{checked:true}).count(),2);
    await button('清空选择').click();
    await page.getByLabel('状态筛选',{exact:true}).selectOption('CURRENT');
    assert.equal(await check(0).count(),0);assert.equal(await check(2).count(),0);
    await page.getByLabel('状态筛选',{exact:true}).selectOption('VOIDED');
    await check(0).waitFor();assert.equal(await page.getByRole('checkbox').count(),2);
    assert(await page.getByRole('row').filter({has:check(0)}).getByRole('button',{name:'调账',exact:true}).isDisabled());
    await page.getByLabel('状态筛选',{exact:true}).selectOption('ALL');
    // Archive is reversible visibility only and never restores a banned card's authorization.
    cards[3].status='banned';cards[4].status='active';
    await button('刷新').click();await check(3).waitFor();
    await check(3).check();await check(4).check();
    const beforeArchive=statusCalls.length;
    page.once('dialog',d=>d.accept());await button('批量归档').click();
    await page.waitForFunction(()=>!document.querySelector('input[type=checkbox]').disabled);
    assert.equal(statusCalls.length,beforeArchive+1);assert.equal(statusCalls.at(-1).action,'archive');
    assert.equal(cards[3].status,'banned');assert.equal(cards[3].pointsAvailable,10);
    await page.getByLabel('状态筛选',{exact:true}).selectOption('CURRENT');assert.equal(await check(3).count(),0);
    await page.getByLabel('状态筛选',{exact:true}).selectOption('ARCHIVED');await check(3).check();
    page.once('dialog',d=>d.accept());await button('取消归档').click();await check(3).waitFor({state:'hidden'});
    assert.equal(statusCalls.at(-1).action,'unarchive');assert.equal(cards[3].status,'banned');assert.equal(cards[3].archivedAt,null);
    await page.getByLabel('状态筛选',{exact:true}).selectOption('ALL');
    // Pending accounting writes cannot be orphaned by deletion, including another operator's deletion.
    cards[5].status='unactivated';
    const pendingAdjustment={operator:'admin',cardId:'bulk-5',delta:10,reason:'pending adjustment test',key:'pending-before-void'};
    await page.evaluate(intent=>sessionStorage.setItem('superkiro.pending-adjustment.v1:admin',JSON.stringify(intent)),pendingAdjustment);
    await page.reload();await page.getByRole('navigation').getByRole('button',{name:'卡密资产',exact:true}).click();
    await button('重新登录确认调账账户').click();await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();
    await page.getByRole('navigation').getByRole('button',{name:'卡密资产',exact:true}).click();await check(5).check();
    const beforePendingVoid=statusCalls.length;
    await button('批量删除（未激活）').click();await page.getByRole('alert').filter({hasText:'本次未发送删除请求'}).waitFor();assert.equal(statusCalls.length,beforePendingVoid);
    cards[5].status='voided';await button('刷新').click();await page.waitForFunction(()=>!document.querySelector('input[type=checkbox]').disabled);
    assert.equal(await check(5).count(),0);
    let replayed=null;await page.route('**/api/v1/admin/cards/adjust',route=>{replayed=route.request().postDataJSON();return route.fulfill({json:{success:true}});});
    await button('核对未确认调账').click();assert(await page.getByLabel('增减积分数量').isDisabled());
    page.once('dialog',d=>d.accept());await button('确认调账').click();await page.getByRole('dialog').waitFor({state:'hidden'});
    assert.equal(replayed.cardId,'bulk-5');assert.equal(replayed.idempotencyKey,'pending-before-void');assert.equal(replayed.deltaPoints,10);
    assert.equal(await page.evaluate(()=>sessionStorage.getItem('superkiro.pending-adjustment.v1:admin')),null);
    await page.getByLabel('状态筛选',{exact:true}).selectOption('ALL');
    // A rejected administrative session must stop the batch, without downloading earlier codes.
    await page.route('**/api/v1/admin/cards/reveal',route=>{revealCalls.push(route.request().postDataJSON().cardId);return route.fulfill({status:401,json:{error:'expired'}});});
    await check(0).check();await check(1).check();page.once('dialog',d=>d.accept());await button('导出已选卡密').click();
    await page.getByLabel('密码',{exact:true}).waitFor();assert.equal(revealCalls.length,3);
    console.log('PASS card bulk: page-only selection, cancel, locking, mixed results, status actions, secret export, refresh and expired session');
  } finally {await browser.close();await new Promise(r=>server.close(r));}
})().catch(error=>{console.error(error);process.exitCode=1;});
