// Local test credentials only. Never use this CA/key for deployment.
const https = require('node:https');
const fs = require('node:fs');
const path = require('node:path');
const {spawnSync} = require('node:child_process');
const dir = path.dirname(process.argv[2]);
const ca = path.join(dir, 'local-test-ca.pem');
const key = path.join(dir, 'local-test-key.pem');
const cert = path.join(dir, 'local-test-cert.pem');
function ensureFixtures() {
  const run = (args) => {
    let result = spawnSync('openssl', args, {cwd: dir, stdio: 'ignore'});
    if (result.error?.code === 'ENOENT' && process.platform === 'win32') {
      result = spawnSync(path.join(process.env.ProgramFiles || 'C:/Program Files', 'Git/usr/bin/openssl.exe'), args, {cwd: dir, stdio: 'ignore'});
    }
    if (result.status !== 0) throw new Error('openssl failed while creating local TLS fixtures');
  };
  const caKey = path.join(dir, 'local-test-ca-key.pem');
  const csr = path.join(dir, 'local-test.csr');
  run(['req','-x509','-newkey','rsa:2048','-nodes','-keyout',caKey,'-out',ca,'-days','2','-subj','/CN=Superkiro Test CA']);
  run(['req','-new','-newkey','rsa:2048','-nodes','-keyout',key,'-out',csr,'-subj','/CN=localhost']);
  run(['x509','-req','-in',csr,'-CA',ca,'-CAkey',caKey,'-CAcreateserial','-out',cert,'-days','2','-sha256','-extfile',path.join(__dirname,'local-test.ext')]);
  for (const file of [caKey, csr, path.join(dir, 'local-test-ca.srl')]) { try { fs.unlinkSync(file); } catch {} }
}
ensureFixtures();
const server = https.createServer({key:fs.readFileSync(key),cert:fs.readFileSync(cert)},(req,res)=>{
  let body=''; req.on('data',c=>body+=c); req.on('end',()=>{
    let result;
    switch(req.url) {
      case '/refreshToken': result={accessToken:'renewed',refreshToken:'refresh',profileArn:'profile',expiresAt:'2099-01-01T00:00:00Z'};break;
      case '/getUsageLimits':
        if(req.headers.authorization !== 'Bearer renewed') {res.writeHead(401);res.end();return;}
        result={success:true};break;
      case '/api/v1/portal/challenge':result={challengeToken:'test-challenge'};break;
      case '/api/v1/portal/unbind':result={success:JSON.parse(body).challenge_token==='test-challenge'};break;
      default:res.writeHead(404);res.end();return;
    }
    res.setHeader('content-type','application/json');res.end(JSON.stringify(result));
  });
});
server.listen(0,'127.0.0.1',()=>fs.writeFileSync(process.argv[2],String(server.address().port)));