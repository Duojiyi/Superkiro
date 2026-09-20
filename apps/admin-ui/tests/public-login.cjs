// Fresh anonymous contexts. Only GET/HEAD requests are allowed, including in production.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
const http=require('node:http'),fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict');
const fixture=require('./fixture-api.cjs')(),root=path.resolve(__dirname,'../dist');
const server=http.createServer(async(req,res)=>{
  if(req.url.startsWith('/api/'))return fixture.handle(req,res);
  const file=path.resolve(root,req.url.split('?')[0].replace(/^\/admin\/?/,'')||'index.html');
  if(!file.startsWith(root+path.sep)||!fs.existsSync(file)){res.writeHead(404);return res.end();}
  res.setHeader('Content-Type',file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':'text/html');res.end(fs.readFileSync(file));
});
(async()=>{
  if(!process.env.ADMIN_PUBLIC_URL)await new Promise(r=>server.listen(0,'127.0.0.1',r));
  const target=process.env.ADMIN_PUBLIC_URL||`http://127.0.0.1:${server.address().port}/admin/`;
  const browser=await chromium.launch({headless:true,...(process.env.CHROME_PATH?{executablePath:process.env.CHROME_PATH}:{})});
  try {
    for(const width of [1440,320]) {
      const context=await browser.newContext({viewport:{width,height:900}}),page=await context.newPage(),sensitive=[];
      await page.route('**/*',route=>{
        const r=route.request(),u=new URL(r.url());
        if(!['GET','HEAD'].includes(r.method())||u.origin!==new URL(target).origin)return route.abort();
        if(u.pathname.startsWith('/api/')&&!u.pathname.endsWith('/admin/session'))sensitive.push(u.pathname);
        return route.continue();
      });
      await page.goto(target,{waitUntil:'networkidle'});
      await page.getByRole('heading',{name:'管理员登录',exact:true}).waitFor();
      assert.equal(await page.locator('.sidebar,.workspace,[role="dialog"],nav').count(),0,'anonymous page must not mount workspace or login overlay');
      assert.equal(await page.getByRole('button',{name:'取消',exact:true}).count(),0);
      assert.equal(await page.getByRole('textbox',{name:'动态验证码'}).count(),0,'disabled 2FA must not show a code field');
      await page.keyboard.press('Escape');
      assert.equal(await page.locator('.sidebar,.workspace,nav').count(),0);
      assert.deepEqual(sensitive,[],'no anonymous business-data requests');
      assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
      const dir=path.resolve(__dirname,'../visual-check/public-login');fs.mkdirSync(dir,{recursive:true});
      await page.screenshot({path:path.join(dir,`${process.env.ADMIN_PUBLIC_URL?'production':'local'}-${width}.png`),fullPage:true});
      await context.close();
    }
    console.log(`PASS: ${process.env.ADMIN_PUBLIC_URL?'production':'local built UI'} fresh anonymous login only at 1440/320px; no sidebar, workspace, cancel bypass or business-data reads; GET/HEAD only`);
  } finally {await browser.close();}
})().catch(e=>{console.error(e);process.exitCode=1;}).finally(()=>server.close());
