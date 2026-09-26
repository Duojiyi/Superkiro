// Authenticated, nonempty local visual and interaction checks. Fixture API is test-only.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
const http=require('node:http'), fs=require('node:fs'), path=require('node:path'), assert=require('node:assert/strict');
const fixture=require('./fixture-api.cjs')();
const root=path.resolve(__dirname,'../dist'), output=path.resolve(__dirname,'../visual-check/pricing-preview');
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
  const page=await browser.newPage({viewport:{width:1440,height:1200},deviceScaleFactor:1});
  const origin='http://127.0.0.1:'+server.address().port;
  page.on('pageerror',error=>errors.push(error.message));
  await page.route('**/*',route=>new URL(route.request().url()).origin===origin?route.continue():route.abort());
  await page.route('**/api/v1/admin/commercial-config',async route=>{
   assert.equal(route.request().method(),'GET','Screenshot must never publish');
   const response=await route.fetch(), body=await response.json();
   body.config.groups[0].margin_multiplier=1.5;body.config.models[0].credit_multiplier=2;
   for(const version of body.config.versions)Object.assign(version,{margin_multiplier:1.2,currency:'USD',input_price_per_m:3,output_price_per_m:15,cache_read_price_per_m:0.3,cache_creation_price_per_m:3.75});
   await route.fulfill({response,json:body});
  });
  await page.goto(origin+'/admin/');
  await page.getByLabel('密码',{exact:true}).fill('fixture-password');
  await page.getByRole('button',{name:'登录',exact:true}).click();
  await page.getByRole('navigation').getByRole('button',{name:'模型与定价',exact:true}).click();
  await page.getByLabel('选择价格版本模板').selectOption('fixture-price-0');
  await page.getByLabel('新版本 ID',{exact:true}).fill('fixture-price-preview-2030');
  await page.getByLabel('生效时间（本地时区）').fill('2030-01-01T10:00');
  await page.getByRole('combobox',{name:/^客户售价单位/}).selectOption('thousand');
  for(const [label,value]of [['未缓存输入','1000'],['输出','500'],['缓存写入','100'],['缓存读取','200']])await page.getByLabel(label+' Tokens',{exact:true}).fill(value);
  await page.getByRole('status').filter({hasText:'预计扣费：0.039366 积分（39366 微积分）'}).waitFor();
  await page.evaluate(()=>{const badge=document.createElement('div');badge.textContent='LOCAL FIXTURE · 本地测试数据 · 未发布';Object.assign(badge.style,{position:'fixed',right:'12px',bottom:'8px',zIndex:'9999',background:'#23272b',color:'white',padding:'8px 12px',fontSize:'12px',pointerEvents:'none'});document.body.append(badge);});
  await page.getByRole('heading',{name:'积分价格',exact:true}).evaluate(el=>el.scrollIntoView({block:'start'}));
  await page.screenshot({path:path.join(output,'pricing-editor-desktop.png')});
  await page.getByRole('heading',{name:'扣费示例',exact:true}).evaluate(el=>el.closest('section').scrollIntoView({block:'center'}));
  await page.screenshot({path:path.join(output,'pricing-charge-preview-desktop.png')});
  await page.getByRole('region',{name:'历史价格版本',exact:true}).evaluate(el=>el.scrollIntoView({block:'center'}));
  await page.screenshot({path:path.join(output,'pricing-history-desktop.png')});
  assert.deepEqual(errors,[]);
  fs.writeFileSync(path.join(output,'results.json'),JSON.stringify({fixtureOnly:true,published:false,viewport:'1440x1200',pricesPerMillion:[3,15,3.75,0.3],tokens:[1000,500,100,200],multipliers:[1.2,1.5,2],expectedMicrocredits:39366,runtimeErrors:errors},null,2));
  console.log('PASS: editor open, thousand-token units, preview 39366 microcredits, readable history screenshots: '+output);
 }finally{await browser.close();}
})().catch(error=>{console.error(error);process.exitCode=1;}).finally(()=>server.close());
