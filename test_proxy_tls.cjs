const assert = require('node:assert/strict');
const fs = require('node:fs');
const net = require('node:net');
const tls = require('node:tls');
const https = require('node:https');
const ca = fs.readFileSync('deploy/server-ca.pem');
const host = '160.202.47.98';
async function tunnel(identity) {
  const socket = net.connect(7897, '127.0.0.1');
  socket.setTimeout(15000, () => socket.destroy(new Error('Proxy timeout')));
  await new Promise((resolve, reject) => {
    let data = '';
    socket.once('error', reject);
    socket.once('connect', () => socket.write(`CONNECT ${host}:443 HTTP/1.1\r\nHost: ${host}:443\r\n\r\n`));
    function onData(chunk) {
      data += chunk;
      if (data.includes('\r\n\r\n')) {
        socket.off('data', onData);
        if (!/^HTTP\/1.[01] 200 /.test(data)) reject(new Error('CONNECT failed'));
        else resolve();
      }
    }
    socket.on('data', onData);
  });
  return await new Promise((resolve, reject) => {
    const secure = tls.connect({socket, ca, ...(identity ? {host: identity} : {})});
    let data = '';
    secure.on('error', reject);
    secure.on('secureConnect', () => secure.write(`GET /healthz HTTP/1.1\r\nHost: ${host}\r\nConnection: close\r\n\r\n`));
    secure.on('data', chunk => data += chunk);
    secure.on('end', () => resolve(data));
    secure.on('close', () => socket.destroy());
  });
}
(async () => {
  await assert.rejects(tunnel(), {code: 'ERR_TLS_CERT_ALTNAME_INVALID'});
  console.log('PASS: original proxy bug reproduced (host omitted)');
  assert.match(await tunnel(host), /^HTTP\/1.1 200 /);
  console.log('PASS: preserved IP host validates certificate and returns health 200');
  await assert.rejects(tunnel('wrong.invalid'), {code: 'ERR_TLS_CERT_ALTNAME_INVALID'});
  console.log('PASS: incorrect host still rejected; TLS identity verification retained');
  await new Promise((resolve, reject) => {
    https.get(`https://${host}/healthz`, {ca, timeout:15000}, res => {assert.equal(res.statusCode,200);res.resume();res.on('end',resolve);}).on('error',reject).on('timeout',function(){this.destroy(new Error('Timeout'));});
  });
  console.log('PASS: direct Node HTTPS certificate validation');
})().catch(error => {console.error(error); process.exitCode=1;});
