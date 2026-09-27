// Providers and Keys: 编辑 / 删除 a provider, the action that fixes a failing Key, names instead of
// IDs, and the hints in the Key editor. Final build + loopback fixture, never production.
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
const key=id=>fixture.keys.find(row=>row.id===id),provider=id=>fixture.providers.find(row=>row.id===id);
const writes=endpoint=>fixture.writes.filter(write=>write.endpoint===endpoint);
const now=Math.floor(Date.now()/1000);
// The OpenAI-format Key is refused as invalid; a provider no model or Key uses any more is left over.
Object.assign(key('fixture-openai-key-1'),{health_state:'unhealthy',last_error:'http_401',last_error_at:now-600});
fixture.providers.push({id:'old-relay',name:'旧中转',base_url:'https://old-relay.invalid',enabled:false,format:'anthropic'});
// A request answered after the primary failed: both providers are named, never their IDs.
fixture.traces.unshift({id:'named-trace',card_id:'fixture-card-0',ts:now-30,invocation_id:'fixture-card-0:named',exposed_model:'gpt-6-astra',status:'success',ttft_ms:400,tokens_per_second:40,
  error_class:null,provider_id:'fixture-openai',input_tokens:100,output_tokens:10,credits_charged:1000,provider_cost_micro_cny:10,
  attempt_chain:[{provider_id:'fixture-provider',key_id:'fixture-key',success:false,error:'HTTP 529 overloaded_error: Overloaded',latency_ms:900},{provider_id:'fixture-openai',key_id:'fixture-openai-key-1',success:true,error:null,latency_ms:700}]});
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
    const confirm=async(accept=true)=>{const box=page.getByRole('alertdialog');await box.waitFor();const text=await box.innerText();
      await (accept?box.locator('[data-confirm="accept"]'):box.getByRole('button',{name:'取消',exact:true})).click();await box.waitFor({state:'detached'});return text;};
    const card=name=>page.locator('.provider-card').filter({has:page.getByRole('heading',{name,exact:true})});
    const menu=async(name,item)=>{await page.getByRole('button',{name:`${name} 的更多操作`,exact:true}).click();await page.getByRole('menuitem',{name:item,exact:true}).click();};
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();await button('刷新').waitFor();
    await nav('供应商与 Key');

    // 编辑供应商: name, address and format; only what changed is sent, the confirmation says what follows.
    await menu('OpenAI 格式 / Fixture','编辑名称、地址和格式');
    const dialog=page.getByRole('dialog',{name:'编辑供应商'});await dialog.waitFor();
    assert.equal(await dialog.getByLabel('接口格式',{exact:true}).inputValue(),'open_ai');
    await dialog.getByRole('button',{name:'保存',exact:true}).click();await dialog.getByRole('alert').filter({hasText:'没有要保存的修改'}).waitFor();
    await dialog.getByLabel('上游地址',{exact:true}).fill('http://astra.invalid');await dialog.getByRole('button',{name:'保存',exact:true}).click();
    await dialog.getByRole('alert').filter({hasText:'上游地址须使用 HTTPS'}).waitFor();assert.equal(writes('providers/update').length,0);
    await dialog.getByLabel('供应商名称',{exact:true}).fill('Astra（OpenAI 格式）');await dialog.getByLabel('上游地址',{exact:true}).fill('https://api.astra.invalid/v1');
    await dialog.getByRole('button',{name:'保存',exact:true}).click();
    const editFacts=await confirm();
    for(const expected of ['名称：OpenAI 格式 / Fixture → Astra（OpenAI 格式）','上游地址：https://openai.invalid/v1 → https://api.astra.invalid/v1','走这个供应商的模型：gpt-6-astra','改用新的地址和格式'])
      assert(editFacts.includes(expected),`${expected}\n${editFacts}`);
    await toasts.shown('已保存供应商 Astra（OpenAI 格式）');await dialog.waitFor({state:'detached'});
    assert.deepEqual(writes('providers/update').at(-1).body,{id:'fixture-openai',name:'Astra（OpenAI 格式）',base_url:'https://api.astra.invalid/v1'});
    await card('Astra（OpenAI 格式）').getByText('https://api.astra.invalid/v1',{exact:true}).waitFor();
    // A format change alone.
    await menu('Astra（OpenAI 格式）','编辑名称、地址和格式');await dialog.getByLabel('接口格式',{exact:true}).selectOption('anthropic');
    toasts.mark();await dialog.getByRole('button',{name:'保存',exact:true}).click();assert((await confirm()).includes('接口格式：OpenAI → Anthropic'));
    await toasts.shown('已保存供应商');
    assert.deepEqual(writes('providers/update').at(-1).body,{id:'fixture-openai',format:'anthropic'});
    console.log('PASS: 编辑供应商 changes the name, address and format, checks them, sends only what changed and says which models follow');

    // 删除供应商: what still uses it is named, and the server's refusal is explained; one nothing uses goes.
    await menu('测试供应商 / Fixture','删除供应商');
    const deleteFacts=await confirm();
    assert(deleteFacts.includes('还有模型的线路用它：')&&deleteFacts.includes('它还有 Key：fixture-key、fixture-backup')&&deleteFacts.includes('服务器会拒绝删除'),deleteFacts);
    await page.getByRole('alert').filter({hasText:'没能删除 测试供应商 / Fixture：还有模型的线路用这个供应商：claude-sonnet、gpt-5、gemini-pro'}).waitFor();
    assert(provider('fixture-provider'));await button('关闭提示').click();
    await menu('旧中转','删除供应商');assert((await confirm()).includes('删除后不能恢复'));
    await toasts.shown('已删除供应商 旧中转');
    assert.equal(provider('old-relay'),undefined);await card('旧中转').waitFor({state:'detached'});
    console.log('PASS: 删除供应商 names the models and Keys that still use it, explains the refusal, deletes one nothing uses');

    // 更换密钥: the editor opens on the API Key field and says what is wrong; after saving, 测试 is offered.
    const openaiRow=card('Astra（OpenAI 格式）').getByRole('row').filter({hasText:'fixture-openai-key-1'});
    await openaiRow.getByRole('button',{name:'更换密钥',exact:true}).click();
    const editor=page.locator('#key-editor');await editor.getByRole('heading',{name:'编辑 Key · fixture-openai-key-1'}).waitFor();
    await editor.getByText('现在：不可用 · HTTP 401 · Key 无效或被拒绝。上游拒绝了这个 Key：填写新的 API Key 并保存',{exact:false}).waitFor();
    assert.equal(await page.evaluate(()=>document.activeElement?.getAttribute('aria-label')),'API Key','the API Key field has focus');
    await editor.getByLabel('API Key',{exact:true}).fill('fixture-new-secret');
    await editor.getByRole('button',{name:'保存',exact:true}).click();assert((await confirm()).includes('将写入这次填写的 API Key'));
    const offer=editor.locator('.secret-saved');await offer.getByText('已换上新密钥：用它测试一次 gpt-6-astra',{exact:false}).waitFor();
    await offer.getByRole('button',{name:'测试',exact:true}).click();
    await offer.getByRole('status').filter({hasText:'成功 · 首字 410 ms'}).waitFor();
    assert.deepEqual(writes('providers/keys/probe').at(-1).body,{provider_id:'fixture-openai',model:'gpt-6-astra',key_id:'fixture-openai-key-1'});
    await openaiRow.getByText('正常',{exact:true}).waitFor();
    assert.equal(await openaiRow.getByRole('button',{name:'更换密钥',exact:true}).count(),0,'healthy again: nothing to fix');
    console.log('PASS: a refused Key offers 更换密钥, which opens the editor on the API Key field; saving a new secret offers 测试 and the Key is healthy again');

    // The Key editor: a model on sale through another provider is not 未上架.
    await editor.getByRole('button',{name:'关闭编辑器',exact:true}).click();
    key('fixture-openai-key-1').allowed_models=[...key('fixture-openai-key-1').allowed_models,'claude-sonnet'];
    await button('刷新').click();await page.locator('.btn-refresh:not([disabled])').waitFor();
    await openaiRow.getByRole('button',{name:'编辑',exact:true}).click();
    const picker=page.getByRole('group',{name:'可用模型'});
    const item=name=>picker.locator('li').filter({has:page.getByRole('checkbox',{name,exact:true})});
    await item('claude-sonnet').getByText('这个供应商还没有线路用它',{exact:true}).waitFor();
    assert.equal(await item('claude-sonnet').getByText('未上架',{exact:true}).count(),0);
    assert.equal(await item('claude-sonnet').getByRole('button',{name:'去上架',exact:true}).count(),0,'listing it again would only duplicate it');
    await item('gpt-5.6-terra').getByText('未上架',{exact:true}).waitFor();
    // 添加供应商: discovery needs a saved provider, and the empty list says so.
    await button('＋ 添加供应商').click();
    const adding=page.locator('#key-editor');await adding.getByRole('heading',{name:'添加供应商'}).waitFor();
    await adding.getByLabel('供应商 ID',{exact:true}).fill('yunwu');
    await adding.getByText('新供应商保存后才能获取模型列表：现在可以点“手动输入”填写模型，也可以先保存，再编辑它的 Key 获取').waitFor();
    assert.equal(await adding.getByText('点“获取模型列表”',{exact:false}).count(),0,'no longer told to press the disabled button');
    await adding.getByRole('button',{name:'关闭编辑器',exact:true}).click();assert((await confirm()).includes('有未保存的修改'));
    console.log('PASS: a model on sale elsewhere reads 这个供应商还没有线路用它 without 去上架; 添加供应商 says discovery needs the provider saved');

    // Provider names, not IDs, on 调用追踪: the filter, the drawer and the attempt chain.
    await nav('调用追踪');
    const filter=page.getByLabel('供应商筛选',{exact:true});
    const options=await filter.locator('option').allTextContents();
    assert.deepEqual(options,['全部','Astra（OpenAI 格式）','测试供应商 / Fixture'],options.join('|'));
    await filter.selectOption({label:'Astra（OpenAI 格式）'});
    assert.equal(await page.evaluate(()=>location.hash),'#/traces?provider=fixture-openai');
    await button('详情').first().click();const drawer=page.locator('#trace-detail');await drawer.waitFor();
    const facts=await drawer.locator('.detail-list').innerText();
    assert(facts.includes('Astra（OpenAI 格式）')&&!facts.includes('fixture-openai'),facts);
    const chain=await drawer.locator('.attempt-list').innerText();
    assert(chain.includes('测试供应商 / Fixture / fixture-key')&&chain.includes('Astra（OpenAI 格式） / fixture-openai-key-1'),chain);
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log('PASS: 调用追踪 names providers in its filter, drawer and attempt chain');
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
