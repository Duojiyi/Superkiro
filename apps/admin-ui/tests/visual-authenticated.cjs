// Authenticated, nonempty local visual and interaction checks. Fixture API is test-only.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
const http=require('node:http'), fs=require('node:fs'), path=require('node:path'), assert=require('node:assert/strict');
const fixture=require('./fixture-api.cjs')();
const root=path.resolve(__dirname,'../dist'), output=path.resolve(__dirname,'../visual-check/authenticated');
fs.mkdirSync(output,{recursive:true});
const errors=[];
const server=http.createServer(async(req,res)=>{
 try {
  if(req.url.startsWith('/api/')) return await fixture.handle(req,res);
  const relative=decodeURIComponent(req.url.split('?')[0]).replace(/^\/admin\/?/,'')||'index.html';
  const file=path.resolve(root,relative);
  if(!file.startsWith(root+path.sep)||!fs.existsSync(file)){res.writeHead(404);return res.end();}
  res.setHeader('Content-Type',file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':'text/html');res.end(fs.readFileSync(file));
 } catch(e){errors.push(e.message);res.writeHead(500);res.end(JSON.stringify({error:e.message}));}
});
(async()=>{
 await new Promise(r=>server.listen(0,'127.0.0.1',r));
 const browser=await chromium.launch({headless:true,...(process.env.CHROME_PATH?{executablePath:process.env.CHROME_PATH}:{})});
 try {
  const page=await browser.newPage({viewport:{width:1440,height:1080},deviceScaleFactor:1});
  page.on('pageerror',e=>errors.push(e.message)); page.on('dialog',d=>d.accept());
  await page.route('**/*',route=>new URL(route.request().url()).origin===`http://127.0.0.1:${server.address().port}`?route.continue():route.abort());
  await page.goto(`http://127.0.0.1:${server.address().port}/admin/`);
  await page.getByLabel('密码',{exact:true}).waitFor();
  await page.locator('input[type=password]').fill('fixture-password');
  await page.getByRole('button',{name:'登录',exact:true}).click();
  await page.getByText('管理员 · 会话有效',{exact:true}).waitFor();
  await page.locator('.metric').filter({hasText:'请求成功率'}).getByText('78.3%',{exact:true}).waitFor();
  assert.equal(await page.locator('.metric').filter({hasText:'成功请求'}).locator('strong').textContent(),'18');
  assert.equal(await page.locator('.chart-column').count(),12);
  assert.equal((await page.locator('.chart-column > span').allTextContents()).reduce((a,b)=>a+Number(b),0),24);
  // Add screenshot-only provenance. No application code or production assets know about fixtures.
  await page.evaluate(()=>{const b=document.createElement('div');b.textContent='LOCAL FIXTURE · 已认证测试数据 · 非生产';Object.assign(b.style,{position:'fixed',bottom:'8px',right:'12px',zIndex:'9999',background:'#23272b',color:'white',padding:'6px 10px',fontSize:'11px',borderRadius:'4px',pointerEvents:'none'});document.body.append(b);});
  const pages=[['overview','运营概览'],['cards','卡密资产'],['providers','供应商与 Key'],['pricing','模型与定价'],['groups','分组与权益'],['trace','调用追踪'],['finance','财务对账'],['security','安全与审计'],['announcements','公告管理']];
  const nav=async name=>{await page.getByRole('navigation').getByRole('button',{name,exact:true}).click();await page.waitForLoadState('networkidle');await page.locator('.page-content').evaluate(e=>e.scrollTop=0);};
  const shot=async name=>{await page.locator('.fixed.top-4').waitFor({state:'hidden'});await page.screenshot({path:path.join(output,name),fullPage:true});};
  for(const [id,name] of pages){
   await nav(name);
   if(id==='pricing'){await page.getByRole('combobox',{name:'选择价格版本模板'}).selectOption('fixture-price-0');assert.equal(await page.getByLabel('输入',{exact:true}).inputValue(),'3');}
   assert.ok(await page.locator('tbody tr').count(),`${id} nonempty table`);
   assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true,`${id} desktop overflow`);
   await shot(`${id}-desktop.png`);
  }
  await nav('卡密资产');
  assert.equal(await page.getByRole('button',{name:'不可恢复',exact:true}).isDisabled(),true);
  await page.getByRole('textbox',{name:'搜索卡密',exact:true}).fill('fixture-card-0');
  assert.equal(await page.locator('tbody tr').count(),1);
  await page.getByRole('button',{name:'查看卡密',exact:true}).click();
  await page.getByLabel('卡密明文',{exact:true}).waitFor();
  assert.equal(await page.getByLabel('卡密明文',{exact:true}).inputValue(),'FIXTURE-RECOVERED-CODE');
  await page.getByRole('button',{name:'关闭并清除',exact:true}).click();
  assert.equal(await page.getByLabel('卡密明文',{exact:true}).count(),0);
  await page.getByRole('button',{name:'冻结',exact:true}).click();
  await page.getByRole('button',{name:'解冻',exact:true}).waitFor();
  await page.getByRole('button',{name:'解冻',exact:true}).click();
  await page.getByRole('button',{name:'冻结',exact:true}).waitFor();
  await page.getByRole('textbox',{name:'搜索卡密',exact:true}).fill('');
  for(const tier of [1000,2000,5000,10000]){
   await page.getByRole('button',{name:'＋ 批量生成',exact:true}).click();
   await page.getByRole('combobox',{name:'积分套餐',exact:true}).selectOption(`tier-${tier}`);
   await page.getByRole('combobox',{name:'权益分组',exact:true}).selectOption('fixture-group-1');
   await page.getByRole('spinbutton',{name:'生成数量',exact:true}).fill('2');
   if(tier===2000)await shot('card-creation-desktop.png');
   await page.getByRole('button',{name:'生成并入库',exact:true}).click();
   await page.getByRole('dialog',{name:'新生成的卡密'}).waitFor();
   assert.ok((await page.getByRole('textbox',{name:'一次性卡密结果'}).inputValue()).includes(`FIXTURE-NOT-VALID-${tier}`));
   if(tier===2000){await shot('card-results-desktop.png');const download=page.waitForEvent('download');await page.getByRole('button',{name:'下载 CSV',exact:true}).click();await download;}
   await page.getByRole('button',{name:'确认并清除',exact:true}).click();
  }
  assert.equal(fixture.writes.filter(w=>w.endpoint==='cards/batch').length,4);
  await nav('调用追踪');
  await page.getByRole('textbox',{name:'筛选请求',exact:true}).fill('fixture-trace-0');
  assert.equal(await page.locator('tbody tr').count(),1);
  await page.getByRole('button',{name:'详情 →',exact:true}).click();
  assert.ok((await page.locator('pre').textContent()).includes('fixture-key'));
  await shot('trace-detail-desktop.png');
  await page.getByRole('textbox',{name:'筛选请求',exact:true}).fill('');
  await nav('财务对账');const exportFile=page.waitForEvent('download');await page.getByRole('button',{name:'导出对账 CSV',exact:true}).click();await exportFile;
  await page.setViewportSize({width:390,height:844});
  for(const[id,name]of pages){await nav(name);assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true,`${id} mobile overflow`);await shot(`${id}-mobile.png`);if(id==='providers'){const scrolled=await page.locator('table').first().evaluate(e=>{e.scrollLeft=e.scrollWidth;return e.scrollLeft>0;});assert.equal(scrolled,true,'mobile provider table actions scroll into view');await shot('providers-mobile-scrolled.png');}}
  await page.setViewportSize({width:1440,height:1080});
  await nav('卡密资产');
  await page.getByRole('textbox',{name:'搜索卡密',exact:true}).fill('fixture-card-0');
  fixture.expire();
  await page.getByRole('button',{name:'刷新',exact:true}).click();
  await page.getByLabel('密码',{exact:true}).waitFor();
  assert.equal(await page.getByRole('navigation').count(),0);
  assert.equal(await page.getByRole('textbox',{name:'搜索卡密',exact:true}).count(),0);
  await page.getByLabel('密码',{exact:true}).fill('fixture-password');
  await page.getByRole('button',{name:'登录',exact:true}).click();
  await page.getByText('管理员 · 会话有效',{exact:true}).waitFor();
  await nav('卡密资产');
  assert.equal(await page.getByRole('textbox',{name:'搜索卡密',exact:true}).inputValue(),'');
  await page.getByRole('button',{name:'退出',exact:true}).click();
  await page.getByLabel('密码',{exact:true}).waitFor();
  assert.equal(await page.locator('tbody').getByText('fixture-card-0',{exact:true}).count(),0);
  assert.deepEqual(errors,[]);
  fs.writeFileSync(path.join(output,'results.json'),JSON.stringify({status:'PASS',fixtureOnly:true,pages:9,viewports:['1440x1080','390x844'],checks:['cookie login/expiry/logout', 'workspace unmounted and filters cleared on expiry', 'reveal/hide', 'unrecoverable disabled','nonempty tables','18 successes / 23 completed = 78.3%','12 chart bins total 24','card search','freeze/unfreeze','all four issuance tiers','one-time card results','CSV downloads','trace filter/detail','no document overflow','no runtime errors'],writes:fixture.writes},null,2));
  console.log(`PASS: authenticated nonempty nine pages, chart metrics, search, freeze/unfreeze, four issuance tiers, CSV exports, trace details, desktop/mobile overflow. ${output}`);
 }finally{await browser.close();}
})().catch(e=>{console.error(e);process.exitCode=1;}).finally(()=>server.close());
