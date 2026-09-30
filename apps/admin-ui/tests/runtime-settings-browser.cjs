// Local authenticated fixture; builds in memory and never contacts an upstream.
const {chromium} = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const assert = require('node:assert/strict'), http = require('node:http'), path = require('node:path');
const fixture = require('./fixture-api.cjs')();
const files = new Map(), posts = [], serverErrors = [];
const profile = {headers_secs:15, attempt_secs:65, total_secs:150, commit_secs:45, started_secs:600, idle_secs:90};
const settings = {standard:{...profile}, reasoning:{...profile, headers_secs:45, attempt_secs:90, idle_secs:180}, claude:{...profile, headers_secs:90, attempt_secs:180, total_secs:300, idle_secs:180}, openai_reasoning_idle_secs:300, keepalive_secs:20};
let config = {revision:'runtime-r1', settings, audit:[]}, readsFail = 1, malformedRead = false, postMode = 'success', releasePost;
const server = http.createServer(async (req, res) => {
  const reply = (value, status = 200) => {res.writeHead(status, {'Content-Type':'application/json'});res.end(JSON.stringify(value));};
  try {
    if (req.url === '/api/v1/admin/runtime-settings') {
      assert(req.headers.cookie?.includes('fixture_session=valid'));
      if (req.method === 'GET') {
        if (readsFail) {readsFail--;return reply({error:'Fixture read unavailable'},500);}
        if (malformedRead) return reply({success:true,config:{...config,audit:null}});
        return reply({success:true,config});
      }
      assert.equal(req.method,'POST');assert.equal(req.headers['x-csrf-token'],'fixture-csrf');
      let raw='';for await (const chunk of req) raw+=chunk;
      const body=JSON.parse(raw);posts.push(body);
      assert.deepEqual(Object.keys(body).sort(),['expected_revision','reason','settings']);
      assert(body.reason.trim());assert.equal(body.reason,body.reason.trim());
      if (postMode==='reject') return reply({error:'Fixture reason rejected'},400);
      if (postMode==='conflict') {
        config={...config,revision:'runtime-conflict',settings:{...config.settings,keepalive_secs:21}};
        return reply({error:'Revision conflict'},409);
      }
      assert.equal(body.expected_revision,config.revision);
      if (postMode==='hold') await new Promise(resolve=>{releasePost=resolve;});
      config={revision:'runtime-r'+(posts.length+1),settings:body.settings,audit:[...config.audit,{revision:'audit-'+posts.length,previous_revision:body.expected_revision,reason:body.reason,created_at_secs:1700000000}]};
      if (postMode==='uncertain') return reply({error:'Response lost after commit'},500);
      if (postMode==='malformed') return reply({success:true,config:{...config,audit:null}});
      return reply({success:true,config});
    }
    if (req.url.startsWith('/api/')) return await fixture.handle(req,res);
    const file=decodeURIComponent(req.url.split('?')[0]).replace(/^\/admin\/?/,'')||'index.html';
    if (!files.has(file)) return reply({error:'Not found'},404);
    res.setHeader('Content-Type',file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':'text/html');res.end(files.get(file));
  } catch (error) {serverErrors.push(error.message);reply({error:error.message},500);}
});
(async()=>{
  let browser;
  try {
    process.chdir(path.resolve(__dirname,'..'));
    const {build}=await import('vite'), {default:react}=await import('@vitejs/plugin-react');
    const result=await build({root:path.resolve(__dirname,'..'),configFile:false,base:'/admin/',plugins:[react()],build:{write:false},logLevel:'warn'});
    for(const output of (Array.isArray(result)?result:[result])) for(const item of output.output) files.set(item.fileName,item.type==='chunk'?item.code:item.source);
    console.log('PASS: production Vite build in memory (dist untouched)');
    await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
    browser=await chromium.launch({headless:true,...(process.env.CHROME_PATH?{executablePath:process.env.CHROME_PATH}:{})});
    const context=await browser.newContext({viewport:{width:1440,height:1000},serviceWorkers:'block'});
    const origin='http://127.0.0.1:'+server.address().port;
    await context.route('**/*',route=>new URL(route.request().url()).origin===origin?route.continue():route.abort());
    const page=await context.newPage(),errors=[];
    page.setDefaultTimeout(12000);page.on('pageerror',error=>errors.push(error.message));
    const button=name=>page.getByRole('button',{name,exact:true});
    const reason=page.getByLabel('修改原因',{exact:true}),keepalive=page.getByLabel(/^客户端保活间隔/);
    const header=page.getByLabel('standard 上游响应头等待',{exact:true});
    const alert=text=>page.getByRole('alert').filter({hasText:text});
    const dialog=page.getByRole('alertdialog');
    const accept=async()=>{await dialog.waitFor();await dialog.locator('[data-confirm="accept"]').click();await dialog.waitFor({state:'detached'});};
    const cancel=async()=>{await dialog.waitFor();await dialog.getByRole('button',{name:'取消',exact:true}).click();await dialog.waitFor({state:'detached'});};
    const publish=async()=>{await button('发布运行参数').click();await accept();};
    const idle=()=>page.waitForFunction(()=>!document.querySelector('fieldset')?.disabled);
    const saved=async()=>{await page.waitForFunction(()=>document.querySelector('fieldset input[maxlength]')?.value==='');await idle();};
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();await button('刷新').waitFor();
    await page.getByRole('navigation').getByRole('button',{name:'运行参数',exact:true}).click();
    await alert('Fixture read unavailable').waitFor();assert.equal(await button('发布运行参数').isDisabled(),true);assert.equal(await reason.count(),0);
    malformedRead=true;await button('重新读取').click();await alert('服务器未返回有效运行配置').waitFor();assert.equal(await reason.count(),0);
    malformedRead=false;await button('重新读取').click();await keepalive.waitFor();await idle();
    assert.equal(new URL(page.url()).hash,'#/runtime');assert.equal(await keepalive.inputValue(),'20');assert.equal(await header.inputValue(),'15');assert.equal(posts.length,0);
    console.log('PASS: failed/malformed loads, retry and loaded server values');

    await keepalive.fill('24');await header.fill('16');await reason.fill('汉'.repeat(342));
    await alert('1024 UTF-8').waitFor();assert.equal(await button('发布运行参数').isDisabled(),true);assert.equal(await reason.isEnabled(),true);
    await reason.fill('bad'+String.fromCharCode(0x85)+'reason');await alert('不可含控制字符').waitFor();assert.equal(await button('发布运行参数').isDisabled(),true);
    await reason.fill('  first publish  ');await header.fill('66');await alert('须满足').waitFor();assert.equal(await button('发布运行参数').isDisabled(),true);await header.fill('16');
    await button('发布运行参数').click();await dialog.waitFor();
    assert.equal(await reason.isDisabled(),true);assert.equal(await keepalive.isDisabled(),true);assert.equal(await header.isDisabled(),true);
    assert.equal(await button('重新读取').isDisabled(),true);assert.equal(await button('刷新').evaluate(el=>!!el.closest('[inert]')),true);
    assert.equal(await button('发布运行参数').isDisabled(),true);assert.equal(posts.length,0);
    // Keyboard input cannot change the captured draft while confirmation is pending.
    await reason.evaluate(el=>el.focus());await page.keyboard.type('staleedit');assert.equal(await reason.inputValue(),'  first publish  ');
    await cancel();await idle();assert.equal(posts.length,0);assert.equal(await keepalive.inputValue(),'24');assert.equal(await reason.isEnabled(),true);
    await reason.fill('  confirmed current draft  ');await keepalive.fill('23');postMode='hold';await publish();
    // Wait for the fixture to receive the POST without relying on a fixed delay.
    for(let i=0;!releasePost&&i<100;i++) await new Promise(resolve=>setTimeout(resolve,10));
    assert.equal(typeof releasePost,'function');assert.equal(await reason.isDisabled(),true);assert.equal(await button('发布运行参数').isDisabled(),true);
    releasePost();releasePost=null;await saved();
    assert.equal(posts.length,1);assert.equal(posts[0].expected_revision,'runtime-r1');assert.equal(posts[0].reason,'confirmed current draft');
    assert.deepEqual(posts[0].settings,{...settings,standard:{...settings.standard,headers_secs:16},keepalive_secs:23});
    assert.equal(await button('发布运行参数').isDisabled(),true);
    console.log('PASS: reason UTF-8/control validation, correction, confirmation cancel/freeze, exact publish payload and request lock');

    postMode='reject';await reason.fill('rejected reason');await keepalive.fill('22');await publish();await alert('发布被拒绝').waitFor();await idle();
    assert.equal(await reason.inputValue(),'rejected reason');assert.equal(await keepalive.inputValue(),'22');assert.equal(await reason.isEnabled(),true);assert.equal(await button('发布运行参数').isEnabled(),true);
    postMode='success';await reason.fill('corrected reason');await publish();await saved();assert.equal(posts.at(-1).reason,'corrected reason');assert.equal(config.settings.keepalive_secs,22);
    console.log('PASS: definite 400 rejection remains editable and retries without reloading');

    postMode='conflict';await reason.fill('preserve conflict draft');await keepalive.fill('24');await publish();await alert('Revision conflict').waitFor();await idle();
    const conflictPosts=posts.length;assert.equal(await reason.inputValue(),'preserve conflict draft');assert.equal(await keepalive.inputValue(),'24');
    assert.equal(await reason.isEnabled(),true);assert.equal(await button('发布运行参数').isDisabled(),true);
    await button('重新读取').click();await cancel();assert.equal(await keepalive.inputValue(),'24');assert.equal(await reason.inputValue(),'preserve conflict draft');assert.equal(posts.length,conflictPosts);
    readsFail=1;await button('重新读取').click();await accept();await alert('Fixture read unavailable').waitFor();await idle();
    assert.equal(await keepalive.inputValue(),'24');assert.equal(await reason.inputValue(),'preserve conflict draft');assert.equal(await button('发布运行参数').isDisabled(),true);
    await button('重新读取').click();await accept();await saved();assert.equal(await keepalive.inputValue(),'21');assert.equal(posts.length,conflictPosts);
    postMode='success';await reason.fill('after explicit reload');await publish();await saved();assert.equal(posts.at(-1).expected_revision,'runtime-conflict');
    console.log('PASS: conflict preserves editable draft, blocks stale retry; cancel/failed reload preserve it; explicit discard refreshes revision');

    for(const mode of ['uncertain','malformed']) {
      postMode=mode;await reason.fill(mode+' publish');await keepalive.fill(mode==='uncertain'?'24':'23');await publish();
      await alert(mode==='uncertain'?'Response lost after commit':'服务器未确认保存').waitFor();await idle();
      const count=posts.length;assert.equal(await reason.inputValue(),mode+' publish');assert.equal(await button('发布运行参数').isDisabled(),true);
      await button('重新读取').click();await accept();await saved();assert.equal(posts.length,count);assert.equal(await keepalive.inputValue(),String(config.settings.keepalive_secs));
    }
    assert.deepEqual(errors,[]);assert.deepEqual(serverErrors,[]);
    console.log('PASS: uncertain/malformed receipts preserve drafts and require GET reconciliation without duplicate POST; no browser/server errors');
  } finally {releasePost?.();await browser?.close();server.close();}
})().catch(error=>{console.error(error);process.exitCode=1;});
