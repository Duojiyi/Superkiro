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
    const nativeDialogs=[];page.on('dialog',d=>{nativeDialogs.push(d.message());void d.dismiss();});
    await page.route('**/*',route=>new URL(route.request().url()).origin===origin?route.continue():route.abort());
    await page.route('**/api/v1/admin/cards?*',route=>route.fulfill({json:{success:true,cards,count:cards.length,revision:'fixture-bulk-1'}}));
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
    const rows=()=>page.getByRole('checkbox',{name:/^选择卡密 /});
    const checked=()=>page.getByRole('checkbox',{name:/^选择卡密 /,checked:true});
    const bar=page.getByRole('region',{name:'批量卡密管理'});
    const barButton=name=>bar.getByRole('button',{name,exact:true});
    const more=async item=>{await bar.getByRole('button',{name:'更多批量操作'}).click();await page.getByRole('menuitem',{name:item,exact:true}).click();};
    const statusTab=value=>page.getByRole('tablist',{name:'状态筛选'}).locator(`[data-value="${value}"]`).click();
    const idle=()=>page.waitForFunction(()=>!document.querySelector('input[aria-label^="选择卡密"]')?.disabled);
    // The console confirms in its own dialog; irreversible bulk actions also need the count typed.
    const confirmation=async(accept,expect=[])=>{
      const box=page.getByRole('alertdialog');await box.waitFor();
      const text=await box.innerText();for(const part of expect)assert(text.includes(part),`confirmation mentions ${part}: ${text}`);
      if(accept){const typed=box.getByLabel('确认输入');if(await typed.count())await typed.fill(await box.locator('.field-label b').innerText());await box.locator('[data-confirm="accept"]').click();}
      else await box.getByRole('button',{name:'取消',exact:true}).click();
      await box.waitFor({state:'detached'});
    };
    await check(0).waitFor();
    // Nothing selected: no bulk actions at all. Selected: 永久作废 is visible without opening 更多, in red.
    assert.equal(await bar.count(),0);
    assert.equal(await rows().count(),50);
    await page.getByLabel('全选本页',{exact:true}).check();assert.equal(await checked().count(),50);
    assert(await barButton('永久作废').isVisible(),'delete remains discoverable with more actions collapsed');
    assert((await barButton('永久作废').getAttribute('class')).includes('btn-danger'));
    await button('下一页').click();assert.equal(await checked().count(),0);
    assert.equal(await rows().count(),5);
    await page.getByLabel('全选本页',{exact:true}).check();await button('取消选择').click();assert.equal(await bar.count(),0);
    await button('上一页').click();await check(0).check();
    await page.getByLabel('搜索卡密',{exact:true}).fill('bulk-1');assert.equal(await checked().count(),0);
    await page.getByLabel('搜索卡密',{exact:true}).fill('');await check(0).check();await check(1).check();
    assert.equal(await checked().count(),2,'both selected cards remain checked after filtering');
    await barButton('冻结').click();await confirmation(false);assert.equal(statusCalls.length,0);
    assert.equal(await checked().count(),2,'cancel preserves both selected cards');
    await barButton('冻结').click();await confirmation(true,['2 张']);
    await waitForRoute(()=>held);assert(await barButton('冻结').isDisabled());assert(await check(0).isDisabled());
    assert(await page.getByLabel('搜索卡密',{exact:true}).isDisabled());
    await barButton('冻结').evaluate(el=>el.click());assert.equal(statusCalls.length,1);
    await held.fulfill({json:{success:true}});
    const results=page.getByLabel('批量操作结果',{exact:true});
    await results.getByText('bulk-1：失败或结果未确认，请刷新核对后再操作',{exact:true}).waitFor();
    await idle();
    assert.deepEqual(statusCalls.map(c=>c.cardId),['bulk-0','bulk-1']);assert.equal(await checked().count(),1);assert(await check(1).isChecked());await button('取消选择').click();
    for(const [action,run] of [['unfreeze',()=>barButton('解冻').click()],['ban',()=>more('封禁')]]){
      cards[3].status=action==='unfreeze'?'frozen':'active';await button('刷新').click();await idle();
      await check(3).check();await run();await confirmation(true);
      await idle();
      assert.equal(statusCalls.at(-1).action,action);assert.equal(statusCalls.at(-1).cardId,'bulk-3');
    }
    await check(0).check();await check(1).check();await check(2).check();
    const downloadPromise=page.waitForEvent('download');await barButton('导出明文').click();await confirmation(true,['明文']);
    const download=await downloadPromise;
    assert.equal(fs.readFileSync(await download.path(),'utf8'),'TEST-ONLY-bulk-0');
    await idle();
    assert.deepEqual(revealCalls,['bulk-0','bulk-1']);
    assert((await results.innerText()).includes('历史卡密不可恢复'));
    assert.equal(await checked().count(),2);assert(await check(1).isChecked());assert(await check(2).isChecked());
    assert.equal(await page.evaluate(()=>JSON.stringify({...localStorage,...sessionStorage}).includes('TEST-ONLY-')),false);
    assert.equal((await page.locator('body').innerText()).includes('TEST-ONLY-'),false);
    await check(0).check();await button('刷新').click();await idle();
    assert.equal(await checked().count(),0);
    // Ineligible states must never be sent; failed selections must not escape a new filter.
    cards[0].status='banned';await button('刷新').click();await idle();
    await check(0).check();const before=statusCalls.length;await barButton('冻结').click();await confirmation(true);
    await idle();
    assert.equal(statusCalls.length,before);assert(await check(0).isChecked());
    await page.getByLabel('搜索卡密',{exact:true}).fill('bulk-4');assert.equal(await checked().count(),0);
    await check(4).check();await more('封禁');await confirmation(true,['1 张']);await idle();
    assert.equal(statusCalls.at(-1).cardId,'bulk-4');assert.equal(statusCalls.length,before+1);
    await page.getByLabel('搜索卡密',{exact:true}).fill('');
    // Delete means irreversible void, and allows activated cards but skips already voided cards.
    cards[0].status='unactivated';cards[1].status='active';cards[2].status='voided';
    await statusTab('ALL');
    await button('刷新').click();await idle();
    assert.equal(await bar.count(),0);
    await check(0).check();await check(1).check();await check(2).check();
    const beforeVoid=statusCalls.length;
    await barButton('永久作废').click();await confirmation(false);assert.equal(statusCalls.length,beforeVoid);
    // The count must be typed before the button works.
    await barButton('永久作废').click();
    const voidBox=page.getByRole('alertdialog');await voidBox.waitFor();
    assert(await voidBox.locator('[data-confirm="accept"]').isDisabled());await voidBox.getByLabel('确认输入').fill('2');
    assert(await voidBox.locator('[data-confirm="accept"]').isDisabled());await voidBox.getByRole('button',{name:'取消',exact:true}).click();
    assert.equal(statusCalls.length,beforeVoid);
    await barButton('永久作废').click();await confirmation(true,['3 张','不能恢复','财务与审计记录保留']);
    await idle();
    assert.equal(statusCalls.length,beforeVoid+2);assert.equal(statusCalls.at(-1).action,'void');assert.equal(statusCalls.at(-1).cardId,'bulk-1');
    assert((await results.innerText()).includes('1 张已永久作废'));
    assert.equal(await checked().count(),2);
    await button('取消选择').click();
    await statusTab('CURRENT');
    assert.equal(await check(0).count(),0);assert.equal(await check(2).count(),0);
    await statusTab('VOIDED');
    await check(0).waitFor();assert.equal(await rows().count(),2);
    assert(await page.getByRole('row').filter({has:check(0)}).getByRole('button',{name:'调账',exact:true}).isDisabled());
    await statusTab('ALL');
    // Archive is reversible visibility only and never restores a banned card's authorization.
    cards[3].status='banned';cards[4].status='active';
    await button('刷新').click();await check(3).waitFor();await idle();
    await check(3).check();await check(4).check();
    const beforeArchive=statusCalls.length;
    await more('归档');await confirmation(true);
    await idle();
    assert.equal(statusCalls.length,beforeArchive+1);assert.equal(statusCalls.at(-1).action,'archive');
    assert.equal(cards[3].status,'banned');assert.equal(cards[3].pointsAvailable,10);
    await statusTab('CURRENT');assert.equal(await check(3).count(),0);
    await statusTab('ARCHIVED');await check(3).check();
    await more('取消归档');await confirmation(true);await check(3).waitFor({state:'hidden'});
    assert.equal(statusCalls.at(-1).action,'unarchive');assert.equal(cards[3].status,'banned');assert.equal(cards[3].archivedAt,null);
    await statusTab('ALL');
    // Pending accounting writes cannot be orphaned by deletion, including another operator's deletion.
    cards[5].status='unactivated';
    const pendingAdjustment={operator:'admin',cardId:'bulk-5',delta:10,reason:'pending adjustment test',key:'pending-before-void'};
    await page.evaluate(intent=>sessionStorage.setItem('superkiro.pending-adjustment.v1:admin',JSON.stringify(intent)),pendingAdjustment);
    // The server names the operator, so the reload needs no second sign-in to find the intent.
    await page.reload();await page.getByRole('navigation').getByRole('button',{name:'卡密资产',exact:true}).click();
    await check(5).check();
    assert(await barButton('永久作废').isVisible());
    const beforePendingVoid=statusCalls.length;
    await barButton('永久作废').click();await page.getByRole('alert').filter({hasText:'本次未发送作废请求'}).waitFor();assert.equal(statusCalls.length,beforePendingVoid);
    assert.equal(await page.getByRole('alertdialog').count(),0);
    cards[5].status='voided';await button('刷新').click();await idle();
    assert.equal(await check(5).count(),0);
    let replayed=null;await page.route('**/api/v1/admin/cards/adjust',route=>{replayed=route.request().postDataJSON();return route.fulfill({json:{success:true}});});
    await page.getByRole('region',{name:'未确认调账恢复'}).getByRole('button',{name:'核对',exact:true}).click();assert(await page.getByLabel('增减积分数量').isDisabled());
    await button('下一步').click();await button('确认入账').click();await page.getByRole('dialog').waitFor({state:'hidden'});
    assert.equal(replayed.cardId,'bulk-5');assert.equal(replayed.idempotencyKey,'pending-before-void');assert.equal(replayed.deltaPoints,10);
    assert.equal(await page.evaluate(()=>sessionStorage.getItem('superkiro.pending-adjustment.v1:admin')),null);
    await statusTab('ALL');
    // A rejected administrative session must stop the batch, without downloading earlier codes.
    await page.route('**/api/v1/admin/cards/reveal',route=>{revealCalls.push(route.request().postDataJSON().cardId);return route.fulfill({status:401,json:{error:'expired'}});});
    await check(0).check();await check(1).check();await barButton('导出明文').click();await confirmation(true);
    await page.getByLabel('密码',{exact:true}).waitFor();assert.equal(revealCalls.length,3);
    assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log('PASS card bulk: page-only selection, cancel, locking, mixed results, status actions, secret export, refresh and expired session');
  } finally {await browser.close();await new Promise(r=>server.close(r));}
})().catch(error=>{console.error(error);process.exitCode=1;});
