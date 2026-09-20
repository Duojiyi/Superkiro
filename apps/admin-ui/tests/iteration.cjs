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
    const page=await browser.newPage({viewport:{width:390,height:844}});
    const origin=`http://127.0.0.1:${server.address().port}`, errors=[];
    page.on('pageerror',e=>errors.push(e.message));
    await page.route('**/*',route=>new URL(route.request().url()).origin===origin?route.continue():route.abort());
    const nav=name=>page.getByRole('button',{name,exact:true}).click();
    const login=async()=>{await page.getByLabel('密码',{exact:true}).fill('fixture-password');await nav('登录');await nav('卡密资产');await page.getByRole('button',{name:'查看卡密',exact:true}).first().waitFor();};
    await page.goto(origin+'/admin/');await login();
    // A committed batch with a lost response must remain locked even after reload.
    let posts=0;
    await page.route('**/api/v1/admin/cards/batch',async route=>{posts++;await route.fetch();await route.abort('failed');});
    await nav('＋ 批量生成');await page.getByLabel('模型与计费分组',{exact:true}).selectOption('fixture-group-0');await page.getByLabel('生成数量',{exact:true}).fill('3');page.once('dialog',d=>d.accept());await nav('生成并入库');
    await page.getByRole('region',{name:'制卡结果核对'}).waitFor();assert.equal(posts,1);
    await page.reload();await nav('卡密资产');
    await page.getByRole('region',{name:'制卡结果核对'}).waitFor();
    await nav('＋ 批量生成');assert(await page.getByRole('button',{name:'生成并入库',exact:true}).isDisabled());await nav('取消');
    await nav('按本批次筛选');
    assert((await page.getByRole('textbox',{name:'搜索卡密',exact:true}).inputValue()).startsWith('批次 '));
    assert.equal(await page.locator('tbody tr').count(),3);
    const unlock=page.getByRole('button',{name:'已核对列表，解除制卡限制',exact:true});assert(await unlock.isDisabled());
    await page.route('**/api/v1/admin/cards?*',route=>route.fulfill({status:503,json:{error:'fixture unavailable'}}));
    await nav('刷新卡密以核对');await page.getByRole('alert').filter({hasText:'卡密核对刷新失败'}).waitFor();assert(await unlock.isDisabled());
    await page.unroute('**/api/v1/admin/cards?*');await nav('刷新卡密以核对');await page.waitForFunction(()=>[...document.querySelectorAll('button')].some(b=>b.textContent==='已核对列表，解除制卡限制'&&!b.disabled));
    page.once('dialog',d=>d.accept());await unlock.click();assert.equal(posts,1);
    assert.equal(await page.evaluate(()=>sessionStorage.getItem('admin-pending-issuance:v1')),null);
    // Save, navigate away and return: only latest server data may initialize the editor.
    let savedKey=null;
    await page.route('**/api/v1/admin/providers',async route=>{const response=await route.fetch();const body=await response.json();if(savedKey)body.keys[0]={...body.keys[0],...savedKey,id:savedKey.key_id};await route.fulfill({json:body});});
    await page.route('**/api/v1/admin/providers/keys',async route=>{savedKey=route.request().postDataJSON();await route.fulfill({json:{success:true}});});
    await nav('供应商与 Key');await page.getByRole('button',{name:'编辑 →',exact:true}).first().click();
    const models=page.getByLabel('允许模型（每行一个精确 ID）');await models.fill('new-model');await page.getByLabel('启用',{exact:true}).uncheck();
    page.once('dialog',d=>d.accept());await nav('确认保存权限');await page.getByRole('status').filter({hasText:'密钥授权已保存'}).waitFor();
    await page.waitForFunction(()=>document.querySelector('table')?.textContent.includes('new-model'));
    await nav('运营概览');await nav('供应商与 Key');await page.waitForFunction(()=>document.querySelector('#key-editor textarea')?.value==='new-model');assert.equal(await models.inputValue(),'new-model');assert.equal(await page.getByLabel('启用',{exact:true}).isChecked(),false);
    await page.route('**/api/v1/admin/providers',route=>route.fulfill({status:503,json:{error:'refresh unavailable'}}));
    await models.fill('saved-despite-refresh-failure');page.once('dialog',d=>d.accept());await nav('确认保存权限');
    await page.getByRole('alert').filter({hasText:'部分数据读取失败'}).waitFor();
    await nav('运营概览');await nav('供应商与 Key');
    await page.waitForFunction(()=>document.querySelector('#key-editor textarea')?.value==='saved-despite-refresh-failure');
    assert.equal(await page.getByLabel('启用',{exact:true}).isChecked(),false);
    await models.fill('unsaved-model');await page.getByRole('button',{name:'编辑 →',exact:true}).first().click();
    page.once('dialog',d=>d.dismiss());await nav('运营概览');assert.equal(await models.inputValue(),'unsaved-model');
    page.once('dialog',d=>d.accept());await nav('调用追踪');
    await page.getByRole('button',{name:'详情 →',exact:true}).first().click();
    await page.waitForFunction(()=>document.activeElement?.id==='trace-detail');
    assert.equal(await page.evaluate(()=>document.activeElement?.id),'trace-detail');
    await nav('财务对账');
    const face=page.getByLabel('积分面值（元 / 积分）');await face.fill('0.8');
    page.once('dialog',d=>d.dismiss());await nav('运营概览');assert.equal(await face.inputValue(),'0.8');
    page.once('dialog',d=>d.accept());await nav('重新读取财务配置');
    await page.waitForFunction(()=>!document.querySelector('fieldset')?.disabled);
    await nav('运营概览');await page.getByRole('heading',{name:'运营概览',exact:true}).waitFor();
    // A known pre-commit insufficient-balance rejection must not lock unrelated cards.
    await nav('安全与审计');await nav('退出登录');await login();
    let adjustments=0;
    await page.route('**/api/v1/admin/cards/adjust',route=>{adjustments++;return route.fulfill({status:409,json:{success:false,error:'Card error: Insufficient credit: available 2000000000 micro-credits, needed 3000000000'}});});
    await page.getByRole('button',{name:'调账',exact:true}).first().click();
    await page.getByLabel('增减积分数量').fill('-3000');await page.getByLabel('调账原因说明').fill('test rejection');
    page.once('dialog',d=>d.accept());await nav('确认调账');
    await page.getByRole('dialog').getByRole('alert').filter({hasText:'服务端明确拒绝调账'}).waitFor();
    assert.equal(await page.getByLabel('增减积分数量').isDisabled(),false);assert.equal(adjustments,1);
    assert.equal(await page.evaluate(()=>Object.keys(sessionStorage).filter(k=>k.includes('pending-adjustment')).length),0);
    await nav('取消');await page.getByRole('button',{name:'调账',exact:true}).nth(1).click();
    assert.equal(await page.getByLabel('增减积分数量').isDisabled(),false);await nav('取消');
    // Unstaged price edits never silently disappear behind a successful publish.
    await nav('模型与定价');await page.getByLabel('选择价格版本模板').selectOption('fixture-price-0');
    await page.getByLabel('新版本 ID',{exact:true}).fill('fixture-new-price');await page.getByLabel('生效时间（本地时区）').fill('2030-01-01T10:00');
    // The legacy fixture omits the version multiplier; enter it explicitly like an administrator.
    await page.getByLabel('价格版本扣费倍率（1 = 不加倍）',{exact:true}).fill('1');
    await page.getByLabel('采购计价币种').selectOption('USD');
    for(const name of ['采购输入价格','采购输出价格','采购缓存读取价格','采购缓存写入价格'])await page.getByLabel(name+'（计价货币 / 百万 Tokens）',{exact:true}).fill('1');
    await page.getByLabel('变更原因',{exact:true}).fill('test new price');
    const config=(await (await page.request.get(origin+'/api/v1/admin/commercial-config')).json()).config;let publishBodies=[];
    await page.route('**/api/v1/admin/commercial-config',route=>{if(route.request().method()!=='POST')return route.continue();const body=route.request().postDataJSON();publishBodies.push(body);return route.fulfill({json:{success:true,config:{...config,versions:[...config.versions,...body.versions]}}});});
    await nav('确认并发布');await page.getByRole('status').filter({hasText:'价格编辑尚未加入发布草稿'}).waitFor();
    assert.equal(publishBodies.length,0);assert.equal(await page.getByLabel('新版本 ID',{exact:true}).inputValue(),'fixture-new-price');
    await nav('加入价格草稿');page.once('dialog',d=>d.accept());await nav('确认并发布');
    await page.getByRole('status').filter({hasText:'发布成功'}).waitFor();assert.equal(publishBodies.length,1);assert.equal(publishBodies[0].versions[0].id,'fixture-new-price');assert.equal(publishBodies[0].versions[0].margin_multiplier,1);
    console.log('PASS: definite insufficient-balance rejection clears only uncommitted intent; another card remains editable; unstaged prices block publish without losing inputs; staged version is submitted');
    assert.deepEqual(errors,[]);
    console.log('PASS: committed issuance lost response locks across reload; failed review cannot unlock; successful review never submits; saved Key remount uses latest permissions; same-Key edit preserves dirty guard; trace details receive focus');
  } finally {await browser.close();}
})().catch(e=>{console.error(e);process.exitCode=1;}).finally(()=>server.close());
