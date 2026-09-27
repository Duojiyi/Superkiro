// 套餐: the catalog cards are issued from, edited under the configuration's revision check (new,
// edit, 下架, 删除 only for a plan no card came from, a plan of several devices kept but not issued
// from); issuance offers the plans on sale, presets the plan's group, warns when another is chosen
// and says what the customer will see; the handout, the CSV and the card drawer carry the plan; an
// unconfirmed publication locks editing; an older server's tiers are shown read-only. Final build +
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
    const posts=[];page.on('request',request=>{const url=new URL(request.url());if(request.method()==='POST'&&['/api/v1/admin/commercial-config','/api/v1/admin/cards/batch'].includes(url.pathname))posts.push({path:url.pathname,body:request.postDataJSON()});});
    await page.route('**/*',route=>new URL(route.request().url()).origin===origin?route.continue():route.abort());
    const button=name=>page.getByRole('button',{name,exact:true});
    const nav=name=>page.getByRole('navigation').getByRole('button',{name,exact:true}).click();
    const refresh=async()=>{await button('刷新').click();await page.locator('.btn-refresh:not([disabled])').waitFor();};
    const table=page.getByRole('region',{name:'套餐列表'});
    const row=name=>table.getByRole('row').filter({has:page.locator('.cell-strong',{hasText:new RegExp(`^${name.replace(/[+()]/g,'\\$&')}$`)})});
    const cells=async name=>(await row(name).locator('td').allInnerTexts()).map(text=>text.replace(/\s+/g,' ').trim());
    const words=async locator=>(await locator.innerText()).replace(/\s+/g,' ').trim();
    const confirmBox=async()=>{const box=page.getByRole('alertdialog');await box.waitFor();return box;};
    const accept=async box=>{await box.locator('[data-confirm="accept"]').click();await box.waitFor({state:'detached'});};
    const published=()=>posts.filter(post=>post.path==='/api/v1/admin/commercial-config');
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();await button('刷新').waitFor();
    await page.evaluate(()=>{window.copied=[];Object.defineProperty(navigator,'clipboard',{configurable:true,value:{writeText:async value=>{window.copied.push(value);}}});});

    // The catalog: the four tiers the server seeds, in order, with the cards issued from each.
    await nav('套餐');await row('PRO').waitFor();
    assert.deepEqual(await table.locator('tbody .cell-strong').allInnerTexts(),['PRO','PRO+','PRO Max','Power']);
    assert.deepEqual(await cells('PRO+'),['PRO+ tier-2000','2,000','¥55.00','30 天','1 台','2','PRO','Kiro Pro+','在售','20','6','编辑 下架 删除']);
    const removePro=row('PRO+').getByRole('button',{name:'删除',exact:true});
    assert(await removePro.isDisabled());assert.equal(await removePro.getAttribute('title'),'已经从它发过 6 张卡，只能下架');
    console.log('PASS: the catalog lists the seeded tiers in order with what Kiro shows and the cards issued from each; one cards came from cannot be deleted');

    // 新建套餐: every field held to the server's bounds; what the customer sees; published with the revision and a reason.
    await button('＋ 新建套餐').click();
    const editor=page.getByRole('dialog',{name:'新建套餐'});await editor.waitFor();
    const fill=async(label,value)=>editor.getByLabel(label,{exact:true}).fill(value);
    await fill('套餐 ID','Trial');await fill('套餐名称','体验卡');await fill('套餐积分','300');await fill('套餐售价','9.999');await fill('有效期天数','7');await fill('并发请求数','1');
    await editor.getByText('只能用 1–64 个小写字母、数字和 -（例：trial-7d）',{exact:true}).waitFor();await editor.getByText('0–100,000 元，最多两位小数',{exact:true}).waitFor();
    await editor.getByRole('button',{name:'发布',exact:true}).click();await editor.getByRole('alert').getByText('请先改正标出的项',{exact:true}).waitFor();
    assert.equal(published().length,0);
    await fill('套餐 ID','trial-7d');await fill('套餐售价','9.9');await editor.getByLabel('默认分组',{exact:true}).selectOption('fixture-group-1');await fill('排序','5');
    assert.equal(await words(editor.getByLabel('客户看到')),'客户看到：套餐「体验卡」· 自定义档位 · 300 积分 · 激活后 7 天有效 · 同时最多 1 个请求');
    await editor.getByRole('button',{name:'发布',exact:true}).click();await editor.getByRole('alert').getByText('请填写变更原因',{exact:true}).waitFor();
    await editor.getByLabel('变更原因',{exact:true}).fill('新增 7 天体验卡');
    await editor.getByRole('button',{name:'发布',exact:true}).click();
    let box=await confirmBox();
    const said=await words(box);
    assert(said.includes('新套餐：套餐「体验卡」· 自定义档位 · 300 积分 · 激活后 7 天有效 · 同时最多 1 个请求')&&said.includes('售价 ¥9.90 · 每张 1 台设备 · 默认分组 PRO+')&&said.includes('原因：新增 7 天体验卡'),said);
    await accept(box);await editor.waitFor({state:'detached'});await row('体验卡').waitFor();
    assert.deepEqual(published().at(-1).body,{expected_revision:'fixture-rev-2',reason:'新增 7 天体验卡',plans:[{id:'trial-7d',name:'体验卡',points:300,price_cny:9.9,validity_days:7,max_devices:1,concurrency:1,
      default_group_id:'fixture-group-1',kiro_plan_type:'CUSTOM',on_sale:true,sort_order:5}]});
    assert.equal(await table.locator('tbody .cell-strong').first().innerText(),'体验卡','sort order 5 comes first');
    console.log('PASS: 新建套餐 checks each field as the server does, shows what the customer will see, and publishes the plan with the revision and a reason');

    // A refusal is said in words and the form stays; a plan of three devices is kept, marked as not issuable yet.
    let refuseNext=true;
    await page.route('**/api/v1/admin/commercial-config',route=>{if(route.request().method()!=='POST'||!refuseNext)return route.fallback();refuseNext=false;
      return route.fulfill({status:409,json:{success:false,error:'Invalid billing state: Unknown default group of plan: family-3'}});});
    await button('＋ 新建套餐').click();await editor.waitFor();
    await fill('套餐 ID','family-3');await fill('套餐名称','家庭');await fill('套餐积分','6000');await fill('套餐售价','120');await fill('设备数','3');
    await editor.getByText('每张卡只能绑定 1 台设备：设备数大于 1 的套餐可以保存，但还不能发卡',{exact:true}).waitFor();
    await fill('变更原因','家庭三台设备');
    await editor.getByRole('button',{name:'发布',exact:true}).click();box=await confirmBox();
    assert((await words(box)).includes('每张卡只能绑定 1 台设备'));await accept(box);
    await editor.getByRole('alert').getByText('服务器拒绝了这次发布：套餐的默认分组不存在（可能刚被改动），请刷新后重选：family-3',{exact:true}).waitFor();
    assert.equal(await editor.getByLabel('套餐 ID',{exact:true}).inputValue(),'family-3','the form is kept');
    await editor.getByRole('button',{name:'发布',exact:true}).click();await accept(await confirmBox());await editor.waitFor({state:'detached'});
    await row('家庭').waitFor();
    assert.equal((await cells('家庭'))[4],'3 台暂不能发卡');
    console.log('PASS: a refusal is said in words with the form kept; a plan of three devices is published and marked 暂不能发卡');

    // 下架: a reason is asked; the plan stays, off sale.
    await row('PRO').getByRole('button',{name:'下架',exact:true}).click();box=await confirmBox();
    assert((await words(box)).includes('下架后不能再从它发卡；已发出的卡照常使用。'));
    assert(await box.locator('[data-confirm="accept"]').isDisabled(),'a reason first');
    await box.locator('#confirm-reason').fill('PRO 停售');await accept(box);
    await row('PRO').getByText('已下架',{exact:true}).waitFor();
    assert.deepEqual(published().at(-1).body.plans.map(plan=>[plan.id,plan.on_sale]),[['tier-1000',false]]);
    console.log('PASS: 下架 publishes the plan off sale, with a reason');

    // Issuance: the plans on sale (one of three devices shown, not choosable); the plan's group preset; a mismatch warned; what the customer sees.
    await nav('卡密资产');await button('＋ 批量生成').click();
    const form=page.getByRole('dialog',{name:'批量生成卡密'});await form.waitFor();
    const planSelect=form.getByLabel('积分套餐',{exact:true}),groupSelect=form.getByLabel('模型与计费分组',{exact:true});
    const options=await planSelect.locator('option').evaluateAll(nodes=>nodes.map(node=>[node.value,node.textContent,node.disabled]));
    assert.deepEqual(options,[['trial-7d','体验卡 · 300 积分 · ¥9.9',false],['tier-2000','PRO+ · 2,000 积分 · ¥55',false],['tier-5000','PRO Max · 5,000 积分 · ¥130',false],
      ['tier-10000','Power · 10,000 积分 · ¥250',false],['family-3','家庭 · 6,000 积分 · ¥120（3 台设备，暂不能发卡）',true]],JSON.stringify(options));
    assert.equal(await planSelect.inputValue(),'tier-2000');assert.equal(await groupSelect.inputValue(),'fixture-group-0','the plan\'s default group');
    await planSelect.selectOption('trial-7d');assert.equal(await groupSelect.inputValue(),'fixture-group-1','the chosen plan\'s group');
    await form.getByLabel('生成数量',{exact:true}).fill('2');
    assert.equal(await words(form.getByLabel('客户看到')),'客户看到：套餐「体验卡」· 自定义档位 · 300 积分 · 激活后 7 天有效 · 同时最多 1 个请求');
    const summary=await words(form.getByLabel('发卡摘要'));
    assert(summary.includes('合计 600 积分 · 售价合计 ¥19.80（按套餐价）')&&summary.includes('有效期 7 天（激活起算）'),summary);
    assert.equal(await form.locator('.batch-mismatch').count(),0);
    await groupSelect.selectOption('fixture-group-2');
    assert.equal(await words(form.locator('.batch-mismatch')),'这个套餐默认发到「PRO+」，这次发到「PRO Max」：卡按这个分组的模型和扣费规则使用');
    await planSelect.selectOption('tier-2000');assert.equal(await groupSelect.inputValue(),'fixture-group-2','a group the operator chose stays');
    await planSelect.selectOption('trial-7d');
    await form.locator('.modal-actions .btn-primary').click();box=await confirmBox();
    const facts=await words(box);
    assert(facts.includes('套餐：体验卡 · 300 积分 · 每张 ¥9.90')&&facts.includes('分组：PRO Max（套餐默认是 PRO+）')&&facts.includes('有效期 7 天（激活起算）')&&facts.includes('客户看到：套餐「体验卡」'),facts);
    await accept(box);
    const done=page.getByRole('dialog',{name:'新生成的卡密'});await done.waitFor();
    const issued=posts.filter(post=>post.path==='/api/v1/admin/cards/batch').at(-1).body;
    assert.deepEqual([issued.planId,issued.templateId,issued.groupId,issued.count],['trial-7d','trial-7d','fixture-group-2',2]);
    assert.equal(await done.locator('.modal-title').innerText(),'已生成 2 张 · 体验卡 300 积分');
    // The handout and the CSV name the plan and its validity.
    await done.getByRole('button',{name:'复制发货文本',exact:true}).click();await page.getByRole('status').filter({hasText:'已复制 2 张卡的发货文本'}).waitFor();
    const handout=i=>`卡密：FIXTURE-NOT-VALID-300-${i}\n套餐：体验卡（300 积分）\n有效期：激活后 7 天\n下载地址：https://kiro.rent`;
    assert.deepEqual(await page.evaluate(()=>window.copied),[handout(0)+'\n\n'+handout(1)]);
    const download=page.waitForEvent('download');await done.getByRole('button',{name:'下载 CSV',exact:true}).click();
    const csv=fs.readFileSync(await (await download).path(),'utf8');
    assert(csv.includes('"fixture-issued-6","FIXTURE-NOT-VALID-300-0","体验卡","300","激活后 7 天","PRO Max",""'),csv);
    await done.getByRole('button',{name:'完成',exact:true}).click();await accept(await confirmBox());await done.waitFor({state:'detached'});
    console.log('PASS: issuance offers the plans on sale, presets the plan\'s group, warns when another is chosen, says what the customer sees, and issues by plan ID; the handout and CSV carry the plan');

    // The card keeps its plan as issued; a card from before the catalog names its tier.
    await page.getByLabel('搜索卡密',{exact:true}).fill('fixture-issued-6');await page.getByRole('row').filter({hasText:'fixture-issued-6'}).first().click();
    const drawer=page.locator('.drawer').filter({hasText:'fixture-issued-6'});
    await drawer.locator('.card-plan').waitFor();
    assert.equal(await words(drawer.locator('.card-plan')),'体验卡 300 积分 · ¥9.90 · 7 天 · 同时 1 个请求 · 自定义档位');
    await button('关闭详情').click();
    await page.getByLabel('搜索卡密',{exact:true}).fill('fixture-card-0');await page.getByRole('row').filter({hasText:'fixture-card-0'}).first().click();
    await page.locator('.drawer').filter({hasText:'fixture-card-0'}).locator('.card-plan').waitFor();
    assert.equal(await words(page.locator('.drawer .card-plan')),'PRO+ Kiro Pro+ · 按发卡积分对应');
    await button('关闭详情').click();
    // A plan cards came from can no longer be deleted; one none came from can.
    await nav('套餐');await row('体验卡').waitFor();
    assert.equal((await cells('体验卡'))[10],'2');assert(await row('体验卡').getByRole('button',{name:'删除',exact:true}).isDisabled());
    await row('家庭').getByRole('button',{name:'删除',exact:true}).click();box=await confirmBox();
    await box.locator('#confirm-reason').fill('不做家庭套餐');await accept(box);
    await row('家庭').waitFor({state:'detached'});
    assert.deepEqual(published().at(-1).body.removed_plans,['family-3']);
    console.log('PASS: a card keeps the plan it was issued from (a card from before the catalog, its tier); a plan none came from is deleted');

    // An issuance refusal is said in words.
    await page.route('**/api/v1/admin/cards/batch',route=>route.fulfill({status:400,json:{__type:'InvalidRequestException',message:'plan is not on sale'}}));
    await nav('卡密资产');await button('＋ 批量生成').click();await form.waitFor();
    await form.locator('.modal-actions .btn-primary').click();await accept(await confirmBox());
    await page.getByRole('alert').filter({hasText:'服务端已拒绝，未生成卡密：这个套餐已下架，不能再发卡：在“套餐”里重新上架，或换一个套餐'}).first().waitFor();
    await page.unroute('**/api/v1/admin/cards/batch');
    if(await form.count())await form.getByRole('button',{name:'取消',exact:true}).click();
    console.log('PASS: an issuance refusal (a plan taken off sale meanwhile) is said in words');

    // An unconfirmed publication locks editing until the list is checked, and is raised on 运营概览.
    await nav('套餐');
    await page.route('**/api/v1/admin/commercial-config',route=>route.request().method()==='POST'?route.abort('connectionreset'):route.fallback());
    await row('PRO').getByRole('button',{name:'上架',exact:true}).click();box=await confirmBox();await box.locator('#confirm-reason').fill('PRO 恢复');await accept(box);
    const panel=page.getByRole('region',{name:'套餐发布结果核对'});await panel.waitFor();
    assert((await words(panel)).includes('可能已经发布：上架套餐「PRO」'));
    assert(await button('＋ 新建套餐').isDisabled());assert(await row('PRO+').getByRole('button',{name:'编辑',exact:true}).isDisabled());
    assert(await panel.getByRole('button',{name:'已核对，继续',exact:true}).isDisabled(),'the list is refreshed first');
    await nav('运营概览');await page.locator('.attention-list').getByRole('button',{name:'上次套餐发布的结果未确认',exact:true}).click();
    await panel.waitFor();await page.unroute('**/api/v1/admin/commercial-config');
    await panel.getByRole('button',{name:'刷新列表',exact:true}).click();
    await panel.getByRole('button',{name:'已核对，继续',exact:true}).click();await accept(await confirmBox());
    await panel.waitFor({state:'detached'});assert(await button('＋ 新建套餐').isEnabled());
    console.log('PASS: an unconfirmed publication locks editing, is raised on 运营概览, and is released only after the list is refreshed');

    // An older server keeps no catalog: its four tiers are shown read-only and still issued from.
    await page.route('**/api/v1/admin/commercial-config',async route=>{if(route.request().method()!=='GET')return route.fallback();
      const response=await route.fetch();const body=await response.json();delete body.config.plans;delete body.config.cards_by_plan;await route.fulfill({json:body});});
    await refresh();
    await page.getByRole('note').filter({hasText:'服务器还不支持编辑套餐'}).waitFor();
    assert.deepEqual(await table.locator('tbody .cell-strong').allInnerTexts(),['PRO','PRO+','PRO Max','Power']);
    assert(await button('＋ 新建套餐').isDisabled());assert.equal((await cells('PRO'))[10],'—');
    await nav('卡密资产');await button('＋ 批量生成').click();await form.waitFor();
    assert.deepEqual(await form.getByLabel('积分套餐',{exact:true}).locator('option').evaluateAll(nodes=>nodes.map(node=>node.value)),['tier-1000','tier-2000','tier-5000','tier-10000']);
    await form.getByRole('button',{name:'取消',exact:true}).click();
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log('PASS: an older server\'s four tiers are shown read-only and still offered for issuance');
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
