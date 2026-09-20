const fs = require('node:fs');
const https = require('node:https');
const assert = require('node:assert/strict');
const output = process.env.KIRO_PROXY_TEST_OUTPUT;
const results = [];
(async () => {
  // Runs in Kiro's Electron Node runtime against its actual packaged modules.
  const {HttpsProxyAgent} = require('https-proxy-agent');
  const {createProxyResolver, createHttpPatch} = require('@vscode/proxy-agent');
  const ca = fs.readFileSync('deploy/server-ca.pem');
  const host = '160.202.47.98';
  const proxy = 'http://127.0.0.1:7897';
  const log = {trace(){},debug(){},info(){},warn(){},error(){}};
  const params = {
    env: {}, log, getLogLevel:()=>0, getProxyURL:()=>proxy,
    getProxySupport:()=> 'override', getNoProxyConfig:()=>['existing.example',host],
    resolveProxy:async()=> 'PROXY 127.0.0.1:7897', proxyResolveTelemetry(){},
    addCertificatesV1:()=>false, addCertificatesV2:()=>false,
    loadAdditionalCertificates:async()=>[], isAdditionalFetchSupportEnabled:()=>false,
  };
  function get(client, options) {
    return new Promise((resolve,reject)=> {
      const req = client.get(`https://${host}/healthz`, {ca, timeout:15000,...options}, res=>{
        res.resume();res.on('end',()=>resolve(res.statusCode));
      });
      req.on('error',reject);req.on('timeout',()=>req.destroy(new Error('Request timeout')));
    });
  }
  await assert.rejects(get(https,{agent:new HttpsProxyAgent(proxy)}),{code:'ERR_TLS_CERT_ALTNAME_INVALID'});
  results.push('PASS: actual Kiro core proxy agent reproduces IP TLS mismatch');
  const resolver=createProxyResolver(params);
  assert.equal((await resolver.resolveProxyByURL(`https://${host}/healthz`)).type,'DIRECT');
  assert.equal((await resolver.resolveProxyByURL('https://existing.example')).type,'DIRECT');
  assert.notEqual((await resolver.resolveProxyByURL('https://unrelated.example')).type,'DIRECT');
  results.push('PASS: bypass scoped to gateway; existing exclusions and unrelated proxy retained');
  const patched=createHttpPatch(params,https,resolver.resolveProxyWithRequest);
  assert.equal(await get(patched,{}),200);
  results.push('PASS: actual Kiro HTTP override + scoped bypass returns HTTPS health 200');
  if (process.argv.includes('--authenticated')) {
    const {accessToken} = JSON.parse(fs.readFileSync(0, 'utf8'));
    async function authenticated(path, method) {
      return new Promise((resolve,reject)=> {
        const req=patched.request(`https://${host}${path}`,{method,ca,timeout:15000,headers:{Authorization:`Bearer ${accessToken}`,'Content-Type':'application/json'}},res=>{
          let body='';res.setEncoding('utf8');res.on('data',part=>body+=part);
          res.on('end',()=>{try{assert.equal(res.statusCode,200);resolve(JSON.parse(body));}catch(error){reject(error);}});
        });
        req.on('error',reject);req.on('timeout',()=>req.destroy(new Error('Request timeout')));
        req.end(method==='POST'?'{}':undefined);
      });
    }
    const usage=await authenticated('/getUsageLimits','GET');
    assert.ok(usage.usageBreakdownList?.length);
    const models=await authenticated('/ListAvailableModels','GET');
    assert.ok(models.models?.length);
    results.push(`PASS: authenticated usage and ${models.models.length} model(s) via actual Kiro HTTP override`);
  }
  const badCa=fs.readFileSync('deploy/server-ca.pem');
  await assert.rejects(get(https,{ca:badCa,servername:'wrong.invalid'}), error => ['ERR_TLS_CERT_ALTNAME_INVALID','EPROTO'].includes(error.code));
  results.push('PASS: incorrect TLS identity remains rejected');
  fs.writeFileSync(output,JSON.stringify({success:true,results},null,2));
  process.exit(0);
})().catch(error=>{fs.writeFileSync(output,JSON.stringify({success:false,results,error:String(error),stack:error.stack},null,2));process.exit(1);});
