// Diagnostic only: runs route-health as committed and with a failure dump, several times each.
const {spawnSync}=require('node:child_process');
const results=[];
for(const [file,times] of [['tests/route-health.cjs',5]])for(let i=1;i<=times;i++){
  const started=Date.now();const run=spawnSync(process.execPath,[file],{encoding:'utf8'});
  const out=(run.stdout||'')+(run.stderr||'');results.push([file,i,run.status,Date.now()-started]);
  console.log(`RUN ${file} #${i}: exit ${run.status} in ${Date.now()-started} ms`);
  if(run.status)console.log(out.split(/\r?\n/).filter(line=>/DIAG|Error|Timeout|assert|PASS/.test(line)).join('\n'));
}
console.log('SUMMARY '+JSON.stringify(results));
