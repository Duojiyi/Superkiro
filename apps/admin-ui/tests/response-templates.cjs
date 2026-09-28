// Local authenticated fixture. Build in memory so shared checkout dist files stay untouched.
const {chromium} = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const assert = require('node:assert/strict'), http = require('node:http'), path = require('node:path');
const fixture = require('./fixture-api.cjs')();
const files = new Map();
let config = {revision: 'templates-r1', rules: [], audit: []}, readsFail = 1, postMode = 'conflict';
const posts = [];
const server = http.createServer(async (req, res) => {
  const reply = (value, status = 200) => {res.writeHead(status, {'Content-Type':'application/json'}); res.end(JSON.stringify(value));};
  try {
    if (req.url === '/api/v1/admin/response-templates') {
      assert(req.headers.cookie?.includes('fixture_session=valid'));
      if (req.method === 'GET') {
        if (readsFail) {readsFail--; return reply({error:'fixture read unavailable'},500);}
        return reply({success:true,config});
      }
      assert.equal(req.method,'POST'); assert.equal(req.headers['x-csrf-token'],'fixture-csrf');
      let raw=''; for await (const chunk of req) raw+=chunk;
      const body=JSON.parse(raw); posts.push(body);
      assert.deepEqual(Object.keys(body).sort(),['expected_revision','reason','rules']);
      assert(body.reason.trim());
      if (postMode === 'conflict') {config={...config,revision:'templates-r2',audit:[{reason:'另一位管理员更新'}]}; return reply({error:'Revision conflict'},409);}
      if (postMode === 'reject') return reply({error:'Fixture validation refusal'},400);
      assert.equal(body.expected_revision,config.revision);
      config={revision:`templates-r${Number(config.revision.split('r').at(-1))+1}`,rules:body.rules,audit:[...config.audit,{reason:body.reason}]};
      if (postMode === 'uncertain') return reply({error:'Response lost after commit'},500);
      return reply({success:true,config});
    }
    if (req.url.startsWith('/api/')) return await fixture.handle(req,res);
    const file=decodeURIComponent(req.url.split('?')[0]).replace(/^\/admin\/?/,'')||'index.html';
    if (!files.has(file)) return reply({error:'Not found'},404);
    res.setHeader('Content-Type',file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':'text/html');res.end(files.get(file));
  } catch (error) {reply({error:error.message},500);}
});
(async()=>{
  let browser;
  try {
    process.chdir(path.resolve(__dirname,'..'));
    const {build}=await import('vite'), {default:react}=await import('@vitejs/plugin-react');
    const result=await build({root:path.resolve(__dirname,'..'),configFile:false,base:'/admin/',plugins:[react()],build:{write:false},logLevel:'warn'});
    for(const output of (Array.isArray(result)?result:[result])) for(const item of output.output) files.set(item.fileName,item.type==='chunk'?item.code:item.source);
    console.log('PASS: production Vite build (in memory, existing dist untouched)');
    await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
    browser=await chromium.launch({headless:true,...(process.env.CHROME_PATH?{executablePath:process.env.CHROME_PATH}:{})});
    const page=await browser.newPage({viewport:{width:1440,height:1000}}), errors=[];
    page.setDefaultTimeout(12000);
    page.on('pageerror',error=>errors.push(error.message));
    const origin=`http://127.0.0.1:${server.address().port}`;
    await page.route('**/*',route=>new URL(route.request().url()).origin===origin?route.continue():route.abort());
    const button=name=>page.getByRole('button',{name,exact:true});
    const nav=name=>page.getByRole('navigation').getByRole('button',{name,exact:true}).click();
    const accept=async()=>{const box=page.getByRole('alertdialog');await box.waitFor();await box.locator('[data-confirm="accept"]').click();await box.waitFor({state:'detached'});};
    const cancel=async()=>{const box=page.getByRole('alertdialog');await box.waitFor();await box.getByRole('button',{name:'取消',exact:true}).click();await box.waitFor({state:'detached'});};
    const variant=i=>page.getByRole('region',{name:`模型变体 ${i}`,exact:true});
    const read=async()=>{await button('读取服务器版本').click();await button('读取服务器版本').waitFor();};
    const publish=async()=>{await button('发布模板').click();await accept();};
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();await button('刷新').waitFor();
    await nav('响应模板');await page.getByRole('alert').filter({hasText:'读取响应模板失败'}).waitFor();
    assert.equal(await button('发布模板').isDisabled(),true);assert.equal(await button('新增规则').count(),0);
    await read();await page.getByText('尚无规则，不拦截任何请求。',{exact:false}).waitFor();assert.equal(posts.length,0);
    assert.equal(new URL(page.url()).hash,'#/templates');
    await button('添加鹈鹕示例（停用、免费）').click();
    assert.equal(await page.getByRole('switch',{name:'启用规则'}).getAttribute('aria-checked'),'false');
    assert.equal(await variant(1).getByLabel(/^固定服务费/).inputValue(),'0');
    assert.equal(await variant(1).getByLabel(/^商业模型/).inputValue(),'');
    await page.getByRole('switch',{name:'启用规则'}).click();
    await variant(1).getByLabel(/^商业模型/).selectOption('gpt-5');
    const code='<!doctype html><script>document.documentElement.dataset.templateExecuted="yes"</script><h1>opaque template</h1>';
    await variant(1).getByLabel(/^完整 HTML 代码/).fill(code);
    await variant(1).getByLabel(/^前置消息/).fill('预设模板，不调用上游。');await variant(1).getByLabel(/^完成消息/).fill('完成，回传不再收费。');
    await button('发布模板').click();await page.getByRole('alert').filter({hasText:'请填写发布原因'}).waitFor();
    await page.getByLabel('发布原因（必填）').fill('发布测试模板');
    await variant(1).getByLabel(/^相对 HTML 文件路径/).fill('../escape.html');await button('发布模板').click();await page.getByRole('alert').filter({hasText:'安全的相对'}).waitFor();
    await variant(1).getByLabel(/^相对 HTML 文件路径/).fill('pages/first.html');
    await variant(1).getByLabel(/^固定服务费/).fill('0.0000001');await button('发布模板').click();await page.getByRole('alert').filter({hasText:'最多 6 位小数'}).waitFor();
    await variant(1).getByLabel(/^固定服务费/).fill('1.234567');
    await button('新增模型变体').click();await variant(2).getByLabel(/^商业模型/).selectOption('claude-sonnet');
    await variant(2).getByLabel(/^商业模型/).locator('option[value="gpt-5"][disabled]').waitFor({state:'attached'});
    await variant(2).getByLabel(/^完整 HTML 代码/).fill('<h1>第二模型的独立代码</h1>');
    await variant(2).getByLabel(/^相对 HTML 文件路径/).fill('pages/second.html');
    await button('新增模型变体').click();await variant(3).getByRole('button',{name:'删除变体',exact:true}).click();await accept();assert.equal(await variant(3).count(),0);
    await button('添加鹈鹕示例（停用、免费）').click();await page.getByLabel('规则名称',{exact:true}).fill('第二条');
    await page.getByLabel(/^匹配方式/).selectOption('contains');await variant(1).getByLabel(/^商业模型/).selectOption('gemini-pro');
    await button('上移规则 2').click();assert((await page.getByRole('region',{name:'模板规则列表'}).locator('ol li').first().innerText()).includes('第二条'));
    await button('下移规则 1').click();await button('删除规则').click();await cancel();assert.equal(await page.getByRole('region',{name:'模板规则列表'}).locator('ol li').count(),2);
    await button('删除规则').click();await accept();assert.equal(await page.getByRole('region',{name:'模板规则列表'}).locator('ol li').count(),1);
    await nav('运营概览');await cancel();assert.equal(await variant(1).getByLabel(/^完整 HTML 代码/).inputValue(),code);
    await page.evaluate(()=>{location.hash='#/overview';});await cancel();assert.equal(new URL(page.url()).hash,'#/templates');
    await button('刷新').click();await page.getByRole('region',{name:'服务器版本核对'}).waitFor();assert.equal(await variant(1).getByLabel(/^完整 HTML 代码/).inputValue(),code);
    await button('保留草稿并采用此版本').click();await accept();
    assert.equal(posts.length,0);assert.equal(await page.evaluate(()=>document.documentElement.dataset.templateExecuted),undefined);
    await publish();await page.getByRole('alert').filter({hasText:'版本冲突'}).waitFor();assert.equal(posts.length,1);assert.equal(await button('发布模板').isDisabled(),true);
    assert.equal(await variant(1).getByLabel(/^完整 HTML 代码/).inputValue(),code);assert.equal(await page.getByLabel('发布原因（必填）').inputValue(),'发布测试模板');
    await read();await page.getByRole('region',{name:'服务器版本核对'}).waitFor();
    assert.equal(await button('发布模板').isDisabled(),true);
    await button('保留草稿并采用此版本').click();await accept();postMode='success';await publish();
    await page.getByText('响应模板已发布',{exact:true}).waitFor();
    assert.equal(posts[1].expected_revision,'templates-r2');assert.equal(posts[1].rules[0].variants[0].price_microcredits,1234567);assert.equal(posts[1].rules[0].variants[1].price_microcredits,0);
    assert.equal(posts[1].rules[0].variants[0].content,code);assert.equal(posts[1].rules[0].enabled,true);
    assert.equal(await page.getByLabel('发布原因（必填）').inputValue(),'');
    console.log('PASS: empty/error load, Chinese navigation, variants, ordering, validation, unsaved guards, refresh protection and explicit revision conflict recovery');
    await page.getByLabel('发布原因（必填）').fill('一微积分测试');await variant(1).getByLabel(/^固定服务费/).fill('0.000001');
    postMode='reject';await publish();await page.getByRole('alert').filter({hasText:'发布被拒绝'}).waitFor();assert.equal(await variant(1).getByLabel(/^固定服务费/).inputValue(),'0.000001');
    postMode='uncertain';await publish();await page.getByRole('alert').filter({hasText:'发布结果未确认'}).waitFor();assert.equal(await button('发布模板').isDisabled(),true);
    const count=posts.length;readsFail=1;await read();await page.getByRole('alert').filter({hasText:'读取响应模板失败'}).waitFor();assert.equal(await button('发布模板').isDisabled(),true);
    await read();await page.getByText('已核对：服务器已保存这次发布',{exact:true}).waitFor();assert.equal(posts.length,count,'read receipt does not re-publish or charge');
    assert.equal(config.rules[0].variants[0].price_microcredits,1);assert.equal(await page.getByLabel('发布原因（必填）').inputValue(),'');
    assert.equal(await page.locator('iframe').count(),0);assert.equal(await page.evaluate(()=>document.documentElement.dataset.templateExecuted),undefined);
    if (process.env.TEMPLATE_SCREENSHOTS) await page.screenshot({path:path.join(__dirname,'templates-desktop.png'),fullPage:true});
    await page.setViewportSize({width:720,height:1000});
    assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false,'narrow console must not overflow');
    if (process.env.TEMPLATE_SCREENSHOTS) await page.screenshot({path:path.join(__dirname,'templates-narrow.png'),fullPage:true});
    await page.setViewportSize({width:1440,height:1000});
    await button('删除规则').click();await accept();await page.getByLabel('发布原因（必填）').fill('关闭模板规则');postMode='success';await publish();await page.getByText('尚无规则，不拦截任何请求。',{exact:false}).waitFor();assert.deepEqual(posts.at(-1).rules,[]);
    assert.deepEqual(errors,[]);
    console.log('PASS: rejection preserves drafts; uncertain commits verified by GET without retry; exact fees, inert HTML, narrow layout and publishing empty rules');
  } finally {await browser?.close();server.close();}
})().catch(error=>{console.error(error);process.exitCode=1;});
