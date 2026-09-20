// Exercise the actual Vite routing locally. No production API or credentials.
const assert = require('node:assert/strict');
const http = require('node:http');
const path = require('node:path');
(async () => {
  const {createServer, loadConfigFromFile} = await import('vite');
  const configFile = path.resolve(__dirname, '../vite.config.ts');
  const {config} = await loadConfigFromFile({command:'serve', mode:'development'}, configFile);
  assert.equal(config.server.proxy['/api'].target, 'http://127.0.0.1:19820');
  assert.equal(config.server.proxy['/admin'], undefined, 'Vite must serve its own admin page');
  const upstream = http.createServer((req, res) => {
    res.setHeader('Content-Type', 'application/json');
    res.end(JSON.stringify({path:req.url, origin:req.headers.origin}));
  });
  let vite;
  try {
    await new Promise(resolve => upstream.listen(0, '127.0.0.1', resolve));
    vite = await createServer({root:path.resolve(__dirname, '..'), configFile, logLevel:'error', optimizeDeps:{noDiscovery:true, include:[]},
      server:{host:'127.0.0.1', port:0, open:false, watch:null, preTransformRequests:false, proxy:{'/api':{target:`http://127.0.0.1:${upstream.address().port}`}}}});
    await vite.listen();
    const base = `http://127.0.0.1:${vite.httpServer.address().port}`;
    const shell = await fetch(base + '/admin/');
    assert.equal(shell.status, 200);
    assert.match(await shell.text(), /@vite\/client/);
    const response = await fetch(base + '/api/v1/admin/session', {headers:{Origin:'https://admin.test'}});
    assert.equal(response.status, 200);
    assert.deepEqual(await response.json(), {path:'/api/v1/admin/session', origin:'https://admin.test'});
    console.log('PASS: local admin shell, API proxy path, and original Origin preserved');
  } finally {
    upstream.closeAllConnections();
    if(vite) {vite.httpServer.closeAllConnections(); await vite.close();}
    if(upstream.listening) await new Promise(resolve => upstream.close(resolve));
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
