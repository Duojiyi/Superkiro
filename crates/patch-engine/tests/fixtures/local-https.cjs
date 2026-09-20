// Local test credentials only. Never use this CA/key for deployment.
const https = require('node:https');
const fs = require('node:fs');
const path = require('node:path');
const server = https.createServer({key:fs.readFileSync(path.join(__dirname,'local-test-key.pem')),cert:fs.readFileSync(path.join(__dirname,'local-test-cert.pem'))},(req,res)=>{
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
