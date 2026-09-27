// 卡密资产: a card's support actions — 解封, the device block (解绑设备, 重置换绑次数), 延长有效期 (one card,
// and in bulk on the filter's results), 编辑备注 and 换分组 — each written to the card's history with its
// detail; refusals in words; a result that did not come back locks further changes until checked.
// Final build + loopback fixture, never production.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
const recordToasts=require('./toasts.cjs');
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
const DAY=86400,now=Math.floor(Date.now()/1000);
// fixture-card-0 has used two of its five unbindings and cools down for five more hours.
Object.assign(fixture.cards[0],{rebindsUsed:2,rebindCooldownUntil:now+5*3600-30});
// Not yet activated: its validity (30 days) counts from activation.
const WAITING='card-7a1c00b2e4d95f31';
fixture.cards.push({id:WAITING,codeRecoverable:true,status:'unactivated',creditTotal:1000000000,creditUsed:0,availableCredits:1000000000,pointsTotal:1000,pointsAvailable:1000,
  boundDevices:[],maxDevices:1,groupId:'fixture-group-0',note:'淘宝 9 月',activationDurationSecs:30*DAY});
const VOIDED='card-0d9e4c1f7a2b6e58';
fixture.cards.push({id:VOIDED,codeRecoverable:true,status:'voided',creditTotal:1000000000,creditUsed:0,availableCredits:1000000000,pointsTotal:1000,pointsAvailable:1000,
  boundDevices:[],maxDevices:1,groupId:'fixture-group-0',note:'测试卡'});
const minute=secs=>{const d=new Date(secs*1000),p=n=>String(n).padStart(2,'0');return `${d.getFullYear()}-${p(d.getMonth()+1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;};
const localDate=secs=>minute(secs).slice(0,10);
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
    const nav=name=>page.getByRole('navigation').getByRole('button',{name,exact:true}).click();
    const idle=()=>page.locator('.btn-refresh:not([disabled])').waitFor();
    const row=id=>page.getByRole('row').filter({has:page.getByLabel(`选择卡密 ${id}`,{exact:true})});
    const drawer=page.locator('#card-detail');
    const open=async id=>{await row(id).locator('.col-group').click();await drawer.locator('.drawer-head').getByText(id,{exact:true}).waitFor();};
    const history=()=>drawer.getByRole('region',{name:'操作记录'}).locator('tbody tr');
    const writes=endpoint=>fixture.writes.filter(write=>write.endpoint===endpoint).map(write=>write.body);
    const toast=text=>toasts.shown(text);
    // A confirmation that needs a reason: accepting is disabled until one is given.
    const confirmWith=async(reason,expect=[])=>{
      const box=page.getByRole('alertdialog');await box.waitFor();const text=await box.innerText();
      for(const part of expect)assert(text.includes(part),`confirmation mentions ${part}: ${text}`);
      assert(await box.locator('[data-confirm="accept"]').isDisabled(),'a reason is required');
      await box.locator('#confirm-reason').fill(reason);await box.locator('[data-confirm="accept"]').click();await box.waitFor({state:'detached'});
    };
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();await button('刷新').waitFor();
    await nav('卡密资产');await row('fixture-card-0').waitFor();

    // The device block: the customer's own unbindings and the cooldown, 重置换绑次数 and 解绑 per device.
    await open('fixture-card-0');
    await drawer.getByText('已换绑 2/5 次 · 冷却剩 5 小时',{exact:true}).waitFor();
    await drawer.getByRole('button',{name:'重置换绑次数',exact:true}).click();
    await confirmWith('客户多次换电脑',['重置 fixture-card-0 的换绑次数？','已换绑 2/5 次 · 冷却剩 5 小时','客户重新有 5 次自助换绑，冷却马上结束']);
    await toast('已重置 fixture-card-0 的换绑次数');
    await drawer.getByText('已换绑 0/5 次',{exact:true}).waitFor();
    assert.equal(await drawer.getByRole('button',{name:'重置换绑次数',exact:true}).count(),0,'nothing left to reset');
    assert.deepEqual(writes('cards/rebinds/reset'),[{cardId:'fixture-card-0',reason:'客户多次换电脑'}]);
    await history().first().filter({hasText:'重置换绑次数'}).waitFor();
    assert(/原已换绑 2 次，冷却到 \d{4}-\d{2}-\d{2} \d{2}:\d{2}/.test(await history().first().innerText()),await history().first().innerText());
    await idle();
    await drawer.getByRole('button',{name:'解绑',exact:true}).click();
    await confirmWith('旧电脑损坏',['解绑设备 fixture-device-0？','客户自己的换绑：已换绑 0/5 次','这台设备马上退出登录','不占用客户的换绑次数']);
    await toast('已解绑设备 fixture-device-0');
    await drawer.locator('.device-block').getByText('未绑定',{exact:true}).waitFor();
    assert.deepEqual(writes('cards/devices/unbind'),[{cardId:'fixture-card-0',deviceId:'fixture-device-0',reason:'旧电脑损坏'}]);
    await history().first().filter({hasText:'解绑设备'}).filter({hasText:'设备 fixture-device-0'}).filter({hasText:'旧电脑损坏'}).waitFor();
    await idle();
    console.log('PASS: the drawer shows 已换绑 2/5 次 · 冷却剩 5 小时; 重置换绑次数 and 解绑 ask for a reason, change the card and are written to its history with their detail');

    // 编辑备注 in place: too long is refused before sending, Escape keeps the drawer, Enter saves.
    const noteRow=drawer.locator('dd').first();
    await noteRow.getByRole('button',{name:'编辑',exact:true}).click();
    const note=noteRow.getByLabel('备注',{exact:true});
    assert.equal(await note.inputValue(),'本地视觉测试数据');
    await note.fill('a'.repeat(257));await noteRow.getByRole('alert').filter({hasText:'备注最多 256 字节（现在 257 字节'}).waitFor();
    assert(await noteRow.getByRole('button',{name:'保存',exact:true}).isDisabled());
    await note.press('Escape');await noteRow.getByRole('button',{name:'编辑',exact:true}).waitFor();assert(await drawer.isVisible(),'Escape leaves the note, not the drawer');
    await noteRow.getByRole('button',{name:'编辑',exact:true}).click();await note.fill('老客户续费 · 张三');await note.press('Enter');
    await toast('已保存备注');await noteRow.getByText('老客户续费 · 张三',{exact:true}).waitFor();
    await row('fixture-card-0').locator('.col-note').getByText('老客户续费 · 张三',{exact:true}).waitFor();
    assert.deepEqual(writes('cards/note'),[{cardId:'fixture-card-0',note:'老客户续费 · 张三'}]);
    await history().first().filter({hasText:'修改备注'}).waitFor();
    await idle();
    console.log('PASS: 编辑备注 in place checks 256 bytes before sending, Escape keeps the drawer open, Enter saves and it is in the history');

    // 换分组: the customer signs in again; a group that stopped taking cards is refused in words.
    await drawer.getByRole('button',{name:'换分组',exact:true}).click();
    const move=page.getByRole('dialog',{name:'换分组'});await move.waitFor();
    assert((await move.innerText()).includes('现在：PRO'));assert((await move.innerText()).includes('客户需要重新登录'));
    assert.deepEqual(await move.getByLabel('新分组',{exact:true}).locator('option:not([disabled])').allTextContents(),['PRO+','PRO Max','Power'],'the other groups that take cards');
    await move.getByLabel('新分组',{exact:true}).selectOption('fixture-group-2');
    const moveButton=move.locator('.modal-actions .btn-primary');assert(await moveButton.isDisabled(),'a reason is required');
    await move.locator('#group-reason').fill('客户升级套餐');
    fixture.config.groups[2].issuance_enabled=false;
    await moveButton.click();await move.getByRole('alert').filter({hasText:'这个分组不接收卡密（没有开启“可发新卡”）：fixture-group-2'}).waitFor();
    delete fixture.config.groups[2].issuance_enabled;
    await moveButton.click();await move.waitFor({state:'detached'});await toast('已把 fixture-card-0 换到 PRO Max');
    await drawer.locator('dd').nth(1).getByText('PRO Max',{exact:false}).waitFor();
    assert.deepEqual(writes('cards/group').at(-1),{cardId:'fixture-card-0',groupId:'fixture-group-2',reason:'客户升级套餐'});
    await history().first().filter({hasText:'换分组'}).filter({hasText:'PRO → PRO Max'}).waitFor();
    await idle();
    console.log('PASS: 换分组 warns that the customer signs in again, lists the groups that take cards, explains a refusal and records PRO → PRO Max');

    // 延长有效期 for one card: +7 天 from its end, previewed; a date earlier than its expiry is refused before sending.
    const until=fixture.cards[0].validUntil;
    await drawer.getByRole('button',{name:'延长',exact:true}).click();
    const extend=page.getByRole('dialog',{name:'延长有效期'});await extend.waitFor();
    assert.equal(await extend.getByRole('radio',{name:'+7 天'}).getAttribute('aria-checked'),'true');
    await extend.getByRole('status').filter({hasText:`到期 ${minute(until)} → ${minute(until+7*DAY)}`}).waitFor();
    await extend.getByRole('radio',{name:'指定日期'}).click();
    await extend.getByLabel('新的到期日期',{exact:true}).fill(localDate(now+2*DAY));
    await extend.getByRole('status').filter({hasText:'不能延期：现在的到期时间更晚'}).waitFor();
    await extend.getByRole('radio',{name:'+7 天'}).click();
    const extendButton=extend.locator('.modal-actions .btn-primary');assert(await extendButton.isDisabled(),'a reason is required');
    await extend.getByRole('button',{name:'服务中断补偿',exact:true}).click();await extendButton.click();
    await extend.waitFor({state:'detached'});await toast('已延长 fixture-card-0 的有效期');
    assert.deepEqual(writes('cards/validity'),[{cardIds:['fixture-card-0'],days:7,reason:'服务中断补偿'}]);
    assert.equal(fixture.cards[0].validUntil,until+7*DAY);
    await history().first().filter({hasText:'延长有效期'}).filter({hasText:`到期改为 ${minute(until+7*DAY)}`}).waitFor();
    await idle();
    // Not yet activated: the days go on its validity from activation; a date does not apply.
    await page.keyboard.press('Escape');await drawer.waitFor({state:'detached'});
    await open(WAITING);await drawer.getByText('激活后起算 · 有效 30 天',{exact:true}).waitFor();
    await drawer.getByRole('button',{name:'延长',exact:true}).click();await extend.waitFor();
    await extend.getByRole('radio',{name:'+30 天'}).click();await extend.getByRole('status').filter({hasText:'激活后有效 30 天 → 60 天'}).waitFor();
    await extend.getByRole('radio',{name:'指定日期'}).click();await extend.getByRole('status').filter({hasText:'不能延期：未激活，只能按天数延长'}).waitFor();
    assert(await extendButton.isDisabled());
    await extend.getByRole('radio',{name:'+30 天'}).click();await extend.locator('#extend-reason').fill('活动赠送');await extendButton.click();
    await extend.waitFor({state:'detached'});await drawer.getByText('激活后起算 · 有效 60 天',{exact:true}).waitFor();
    await history().first().filter({hasText:'激活后有效 60 天'}).waitFor();
    await idle();
    console.log('PASS: 延长有效期 previews the new expiry, refuses an earlier date before sending, and lengthens a waiting card\'s validity from activation');

    // 解封 needs a reason; the customer signs in again. A refusal is explained and locks nothing.
    await page.keyboard.press('Escape');await drawer.waitFor({state:'detached'});
    await row('fixture-card-3').getByRole('button',{name:'更多操作'}).click();
    assert.deepEqual(await page.getByRole('menuitem').allInnerTexts(),['解封'],'a banned card can be unbanned from its row');
    await page.keyboard.press('Escape');
    await open('fixture-card-3');
    await page.route('**/api/v1/admin/cards/status',route=>route.fulfill({status:400,contentType:'application/json',body:JSON.stringify({success:false,error:'Invalid billing state: cannot unban Active'})}),{times:1});
    await drawer.getByRole('button',{name:'解封',exact:true}).click();
    await confirmWith('误封',['解封卡密 fixture-card-3？','恢复为使用中','封禁时退出的登录不会恢复：客户需要重新登录']);
    await page.getByRole('alert').filter({hasText:'没有解封：只有已封禁的卡可以解封（这张卡现在是使用中）'}).waitFor();
    assert.equal(await page.getByRole('region',{name:'卡密修改结果核对'}).count(),0,'a refusal changed nothing: nothing to check');
    await drawer.getByRole('button',{name:'解封',exact:true}).click();await confirmWith('误封，已核实');
    await toast('已解封 fixture-card-3');await drawer.locator('.drawer-head').getByText('使用中',{exact:true}).waitFor();
    assert.deepEqual(writes('cards/status').filter(body=>body.action==='unban').at(-1),{cardId:'fixture-card-3',action:'unban',reason:'误封，已核实'});
    await history().first().filter({hasText:'解封'}).filter({hasText:'误封，已核实'}).waitFor();
    await idle();
    console.log('PASS: 解封 from the row menu or the drawer needs a reason, says the customer signs in again, explains a refusal and is in the history');

    // The server could not save (503 in its words): nothing changed, nothing to check.
    fixture.cards[2].rebindsUsed=1;await button('刷新').click();await idle();
    await page.keyboard.press('Escape');await drawer.waitFor({state:'detached'});await open('fixture-card-2');
    await page.route('**/api/v1/admin/cards/rebinds/reset',route=>route.fulfill({status:503,contentType:'application/json',body:JSON.stringify({success:false,error:'The change could not be saved, so nothing was changed; retry shortly'})}),{times:1});
    await drawer.getByRole('button',{name:'重置换绑次数',exact:true}).click();await confirmWith('客户要求');
    await page.getByRole('alert').filter({hasText:'没有重置：服务器没能保存这次修改，什么都没有改：请稍后重试'}).waitFor();
    assert.equal(await page.getByRole('region',{name:'卡密修改结果核对'}).count(),0);
    // A lost reply may have been applied: everything that changes a card waits until the list is checked.
    await page.route('**/api/v1/admin/cards/note',route=>route.abort('failed'),{times:1});
    await noteRow.getByRole('button',{name:'编辑',exact:true}).click();await note.fill('等核对');await note.press('Enter');
    await page.getByRole('alert').filter({hasText:'没收到修改备注的结果'}).waitFor();
    const recovery=page.getByRole('region',{name:'卡密修改结果核对'});await recovery.waitFor();
    assert((await recovery.innerText()).includes('修改备注'));
    for(const name of ['编辑','换分组','延长','解绑','重置换绑次数']){
      const control=drawer.getByRole('button',{name,exact:true});
      if(await control.count())assert(await control.first().isDisabled(),`${name} waits for the check`);
    }
    assert.equal(await drawer.getByRole('button',{name:'编辑',exact:true}).getAttribute('title'),'上次的卡密修改结果未确认，请先在列表上方核对');
    assert(await page.evaluate(()=>!!sessionStorage.getItem('admin-pending-card-change:v1')),'kept for a reload');
    await page.reload();await nav('卡密资产');await recovery.waitFor();
    await nav('运营概览');await page.locator('.attention-list').getByText('上次的卡密修改结果未确认',{exact:false}).waitFor();
    await nav('卡密资产');
    const release=recovery.getByRole('button',{name:'已核对，继续',exact:true});assert(await release.isDisabled());
    await recovery.getByRole('button',{name:'刷新列表',exact:true}).click();await page.waitForFunction(()=>![...document.querySelectorAll('button')].find(b=>b.textContent==='已核对，继续')?.disabled);
    await release.click();const box=page.getByRole('alertdialog');await box.waitFor();await box.locator('[data-confirm="accept"]').click();
    await recovery.waitFor({state:'detached'});
    assert(await page.evaluate(()=>!sessionStorage.getItem('admin-pending-card-change:v1')));
    console.log('PASS: a 503 that says nothing was saved is explained and locks nothing; a lost reply locks every card change (kept across a reload, raised on 运营概览) until the list is refreshed and checked');

    // In bulk: the ticked cards or all a filter finds; the ones that cannot be extended are left out and counted.
    await page.getByRole('tablist',{name:'状态筛选'}).locator('[data-value="ALL"]').click();
    await page.getByRole('checkbox',{name:'全选本页',exact:true}).check();
    const bar=page.getByRole('region',{name:'批量卡密管理'});
    await bar.getByRole('button',{name:'延长有效期',exact:true}).click();
    const bulk=page.getByRole('dialog',{name:'延长有效期'});await bulk.waitFor();
    const total=fixture.cards.length;
    assert((await bulk.locator('.modal-title').innerText()).includes(`延长 ${total} 张卡的有效期`));
    await bulk.getByRole('radio',{name:'+3 天'}).click();
    const review=await bulk.getByRole('status').innerText();
    assert(review.includes(`将延长 ${total-1} 张，每张 +3 天`)&&review.includes('跳过 1 张（已作废 1 张）'),review);
    // The list is older than the server: one card was archived meanwhile. All or nothing, and named.
    const before=fixture.cards.map(card=>card.validUntil);fixture.cards[5].archivedAt=now;
    await bulk.locator('#extend-reason').fill('9/26 上游中断补偿');await bulk.locator('.modal-actions .btn-primary').click();
    await bulk.getByRole('alert').filter({hasText:'已归档的卡要先取消归档，再延期：fixture-card-5'}).waitFor();
    assert.deepEqual(fixture.cards.map(card=>card.validUntil),before,'nothing was extended');
    delete fixture.cards[5].archivedAt;
    await bulk.locator('.modal-actions .btn-primary').click();await bulk.waitFor({state:'detached'});await toast(`已延长 ${total-1} 张卡的有效期`);
    const sent=writes('cards/validity').at(-1);
    assert.equal(sent.days,3);assert.equal(sent.cardIds.length,total-1);assert(!sent.cardIds.includes(VOIDED),'the voided card is not sent');
    await idle();
    // More than a page: 选择全部 N 张筛选结果 extends every one of them.
    for(let i=0;i<55;i++)fixture.cards.push({id:`bulk-extend-${i}`,codeRecoverable:true,status:'active',creditTotal:1000000000,creditUsed:0,availableCredits:1000000000,pointsTotal:1000,pointsAvailable:1000,
      boundDevices:[],maxDevices:1,activatedAt:now-DAY,validUntil:now+10*DAY,groupId:'fixture-group-1',note:'批量延期'});
    await button('刷新').click();await idle();
    await page.getByLabel('搜索卡密',{exact:true}).fill('bulk-extend');
    await page.getByRole('checkbox',{name:'全选本页',exact:true}).check();await bar.getByRole('button',{name:'选择全部 55 张筛选结果',exact:true}).click();
    await bar.getByRole('button',{name:'延长有效期',exact:true}).click();await bulk.waitFor();
    assert((await bulk.innerText()).includes('全部 55 张筛选结果（不只本页）'));
    await bulk.getByRole('radio',{name:'+1 天'}).click();await bulk.getByRole('button',{name:'活动赠送',exact:true}).click();
    await bulk.locator('.modal-actions .btn-primary').click();await bulk.waitFor({state:'detached'});await toast('已延长 55 张卡的有效期');
    assert.equal(writes('cards/validity').at(-1).cardIds.length,55);
    assert(fixture.cards.filter(card=>card.id.startsWith('bulk-extend-')).every(card=>card.validUntil===now+11*DAY));
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log('PASS: bulk 延长有效期 on the ticked cards or all 55 a filter finds, leaving out and counting the ones that cannot be extended; a refusal names the card and changes nothing');
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
