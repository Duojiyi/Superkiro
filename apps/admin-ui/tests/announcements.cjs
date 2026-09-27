// 公告管理: an announcement shown from a start to an end (or until withdrawn), to every customer or the
// cards of some groups, with the note on what the client can see; a preview that looks and pages as
// the customer's client shows it; an edit of one published, only what changed sent and each edit
// listed; ended and withdrawn ones on request; an older server's announcements as before. Final
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
const minute=secs=>{const d=new Date(secs*1000);return `${d.getFullYear()}-${pad(d.getMonth()+1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;};
const minuteStart=secs=>{const d=new Date(secs*1000);d.setSeconds(0,0);return Math.floor(d.getTime()/1000);};
const moment=secs=>minute(secs).slice(5).replace('T',' ');
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
    const posts=[];page.on('request',request=>{const url=new URL(request.url());if(request.method()==='POST'&&url.pathname.startsWith('/api/v1/admin/announcements'))posts.push({path:url.pathname.replace('/api/v1/admin/',''),body:request.postDataJSON()});});
    await page.route('**/*',route=>new URL(route.request().url()).origin===origin?route.continue():route.abort());
    const button=name=>page.getByRole('button',{name,exact:true});
    const nav=name=>page.getByRole('navigation').getByRole('button',{name,exact:true}).click();
    const refresh=async()=>{await button('刷新').click();await page.locator('.btn-refresh:not([disabled])').waitFor();};
    const words=async locator=>(await locator.innerText()).replace(/\s+/g,' ').trim();
    const list=page.getByRole('region',{name:'公告列表'});
    const row=title=>list.locator('tbody tr').filter({has:page.getByRole('button',{name:title,exact:true})});
    const cells=async title=>(await row(title).locator('td').allInnerTexts()).map(text=>text.replace(/\s+/g,' ').trim());
    const confirmBox=async()=>{const box=page.getByRole('alertdialog');await box.waitFor();return box;};
    const accept=async box=>{await box.locator('[data-confirm="accept"]').click();await box.waitFor({state:'detached'});};
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();await button('刷新').waitFor();
    await nav('公告管理');await row('本地测试：服务维护通知').waitFor();
    const standing=await cells('本地测试：服务维护通知');
    assert.deepEqual([standing[1],standing[2],standing[4],standing[5]],['普通','全部客户','不自动结束','生效中'],standing.join(' | '));

    // A notice for tomorrow night's maintenance, for the PRO+ cards only.
    const now=Math.floor(Date.now()/1000),start=now+2*3600,end=now+4*3600;
    await page.getByLabel('公告标题',{exact:true}).fill('今晚服务维护');await page.getByLabel('公告等级',{exact:true}).selectOption('warning');
    await page.getByLabel('正文内容',{exact:true}).fill('02:00–04:00 服务维护，期间请求可能失败。');
    await page.getByLabel('开始',{exact:true}).selectOption('at');await page.getByLabel('开始时间',{exact:true}).fill(minute(start));
    await page.getByLabel('有效期',{exact:true}).selectOption('at');await page.getByLabel('结束时间',{exact:true}).fill(minute(start-60));
    await button('发布').click();await page.getByRole('alert').filter({hasText:'结束时间要晚于开始时间'}).waitFor();
    await page.getByLabel('结束时间',{exact:true}).fill(minute(end));
    await page.getByLabel('对象',{exact:true}).selectOption('groups');
    assert.equal(await words(page.getByLabel('显示范围')),'请至少选一个分组','no group chosen is not everyone');
    await button('发布').click();await page.getByRole('alert').filter({hasText:'请至少选一个分组'}).waitFor();
    assert.equal(posts.length,0,'nothing is sent until the notice is right');
    await page.getByRole('group',{name:'分组'}).getByLabel('PRO+',{exact:true}).check();
    await page.getByText('目前发布的客户端版本拉取公告时不带卡的登录信息',{exact:false}).waitFor();
    // The preview is the client's dialog: 公告, 关闭, the level as the client words it, the title and text.
    const preview=page.locator('section.panel').filter({has:page.getByRole('heading',{name:'预览',exact:true})}).getByRole('group',{name:'客户端预览'});
    const previewText=await words(preview);
    assert(previewText.startsWith('公告 关闭 第 1 / 1 条 · 重要 今晚服务维护 02:00–04:00 服务维护，期间请求可能失败。'),previewText);
    assert.equal(await words(page.getByLabel('显示范围')),`只对 PRO+ 分组的卡可见 · ${moment(start)} 开始 · ${moment(end)} 结束`);
    await button('发布').click();let box=await confirmBox();
    const said=await words(box);
    assert(said.includes('今晚服务维护')&&said.includes(`只对 PRO+ 分组的卡可见 · ${moment(start)} 开始 · ${moment(end)} 结束`)&&said.includes('目前发布的客户端版本'),said);
    await accept(box);await row('今晚服务维护').waitFor();
    assert.deepEqual(posts.at(-1),{path:'announcements',body:{title:'今晚服务维护',content:'02:00–04:00 服务维护，期间请求可能失败。',level:'warning',startsAtSecs:minuteStart(start),endsAtSecs:minuteStart(end),audience:['fixture-group-1']}});
    const scheduled=await cells('今晚服务维护');
    assert.deepEqual([scheduled[1],scheduled[2],scheduled[5]],['预警','PRO+','未开始']);
    assert(scheduled[3].startsWith(moment(minuteStart(start)))&&scheduled[3].includes('后）'),scheduled[3]);
    assert.equal(scheduled[4],moment(minuteStart(end)));
    console.log('PASS: a notice from a start to an end for some groups: checked before sending, previewed as the client shows it, the note on what the client sees, published with its window and audience');

    // Edit: only what changed is sent, and the edit is listed.
    await row('今晚服务维护').getByRole('button',{name:'编辑',exact:true}).click();
    const editor=page.getByRole('dialog',{name:'编辑公告'});await editor.waitFor();
    await editor.getByRole('button',{name:'保存修改',exact:true}).click();await editor.getByRole('alert').getByText('没有修改',{exact:true}).waitFor();
    await editor.getByLabel('修改标题',{exact:true}).fill('今晚服务维护（延后一小时）');
    await editor.getByLabel('结束时间',{exact:true}).fill(minute(end+3600));
    await editor.getByRole('button',{name:'保存修改',exact:true}).click();box=await confirmBox();
    assert((await words(box)).includes('改了标题、结束时间'));await accept(box);
    await editor.waitFor({state:'detached'});await row('今晚服务维护（延后一小时）').waitFor();
    const id=fixture.notices.find(notice=>notice.title==='今晚服务维护（延后一小时）').id;
    assert.deepEqual(posts.at(-1),{path:'announcements/edit',body:{id,title:'今晚服务维护（延后一小时）',endsAtSecs:minuteStart(end+3600)}});
    await row('今晚服务维护（延后一小时）').getByRole('button',{name:'今晚服务维护（延后一小时）',exact:true}).click();
    const history=list.getByRole('list',{name:'修改记录'});await history.waitFor();
    assert.match(await words(history),/^admin \d\d-\d\d \d\d:\d\d 改了标题、结束时间$/);
    // A refusal keeps the editor open, in words.
    await page.route('**/api/v1/admin/announcements/edit',route=>route.fulfill({status:409,json:{success:false,error:'A withdrawn announcement cannot be edited'}}));
    await row('今晚服务维护（延后一小时）').getByRole('button',{name:'编辑',exact:true}).click();await editor.waitFor();
    await editor.getByLabel('修改等级',{exact:true}).selectOption('critical');
    await editor.getByRole('button',{name:'保存修改',exact:true}).click();await accept(await confirmBox());
    await editor.getByRole('alert').getByText('服务端已拒绝，公告没有改动：已撤回的公告不能再编辑',{exact:true}).waitFor();
    // No answer at all: it may have been saved; publishing waits until the list is checked.
    await page.unroute('**/api/v1/admin/announcements/edit');
    await page.route('**/api/v1/admin/announcements/edit',route=>route.abort('connectionreset'));
    await editor.getByRole('button',{name:'保存修改',exact:true}).click();await accept(await confirmBox());
    const recovery=page.getByRole('region',{name:'公告发布结果核对'});await recovery.waitFor();
    await page.getByRole('alert').filter({hasText:'没收到修改结果'}).first().waitFor();
    assert(await button('发布').isDisabled());
    await page.unroute('**/api/v1/admin/announcements/edit');
    await recovery.getByRole('button',{name:'刷新列表',exact:true}).click();
    await recovery.getByRole('button',{name:'已核对，继续',exact:true}).click();await accept(await confirmBox());
    await recovery.waitFor({state:'detached'});assert(await button('发布').isEnabled());
    console.log('PASS: an edit sends only what changed and is listed with who and when; a refusal is said in words; no answer locks publishing until the list is checked');

    // Withdrawn and ended ones are listed on request.
    await row('本地测试：服务维护通知').getByRole('button',{name:'撤回',exact:true}).click();await accept(await confirmBox());
    await row('本地测试：服务维护通知').waitFor({state:'detached'});
    assert.deepEqual(posts.at(-1),{path:'announcements/withdraw',body:{id:'fixture-notice'}});
    await list.getByLabel('显示已结束和已撤回',{exact:true}).check();
    await row('本地测试：服务维护通知').waitFor();
    assert.equal((await cells('本地测试：服务维护通知'))[5],'已撤回');
    assert.equal(await row('本地测试：服务维护通知').getByRole('button',{name:'编辑',exact:true}).count(),0);
    await list.getByLabel('显示已结束和已撤回',{exact:true}).uncheck();await row('本地测试：服务维护通知').waitFor({state:'detached'});
    console.log('PASS: a withdrawn notice leaves the list, and is shown with ended ones on request, without edit');

    // A long text pages in the preview as the client pages it.
    await page.getByLabel('公告标题',{exact:true}).fill('长公告');
    await page.getByLabel('正文内容',{exact:true}).fill(Array.from({length:40},(_,i)=>`第 ${i+1} 行：维护期间请求可能失败，请稍后重试。`).join('\n'));
    const counter=preview.locator('.client-notice-pagination span');
    await page.waitForFunction(()=>/^1 \/ [2-9] 页$/.test(document.querySelector('.client-notice-pagination span')?.textContent?.trim()??''));
    const pagesText=await counter.innerText();
    await preview.getByRole('button',{name:'预览下一页',exact:true}).click();
    assert.equal(await counter.innerText(),pagesText.replace(/^1/,'2'));
    await page.getByLabel('公告标题',{exact:true}).fill('');await page.getByLabel('正文内容',{exact:true}).fill('');
    console.log('PASS: a long notice pages in the preview as the client pages it');

    // An older server: no status, no start, no audience; its notices and publishing are as before.
    await page.route('**/api/v1/admin/announcements',async route=>{if(route.request().method()!=='GET')return route.fallback();
      const response=await route.fetch();const body=await response.json();
      await route.fulfill({json:{...body,announcements:body.announcements.map(({status,starts_at,audience,edits,...rest})=>rest)}});});
    await page.route('**/api/v1/admin/stats',async route=>{const response=await route.fetch();const body=await response.json();delete body.persistenceReady;delete body.persistenceError;delete body.lastSavedAtSecs;await route.fulfill({json:body});});
    await refresh();
    await page.getByText('服务器还不支持定时、指定结束时间和按分组发布',{exact:false}).waitFor();
    assert(await page.getByLabel('开始',{exact:true}).isDisabled());assert(await page.getByLabel('对象',{exact:true}).isDisabled());
    assert.deepEqual(await page.getByLabel('有效期',{exact:true}).locator('option').allInnerTexts(),['1 天','3 天','7 天','30 天']);
    assert.equal(await list.getByRole('button',{name:'编辑',exact:true}).count(),0);
    await page.getByLabel('公告标题',{exact:true}).fill('旧服务器公告');await page.getByLabel('正文内容',{exact:true}).fill('立即显示给全部客户');await page.getByLabel('公告等级',{exact:true}).selectOption('info');
    await page.getByLabel('有效期',{exact:true}).selectOption('3');
    await button('发布').click();await accept(await confirmBox());
    await row('旧服务器公告').waitFor();
    assert.deepEqual(posts.at(-1).body,{title:'旧服务器公告',content:'立即显示给全部客户',level:'info',ttlSecs:3*86400});
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log('PASS: with an older server the notices and publishing are as before: now, to everyone, for some days');
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
