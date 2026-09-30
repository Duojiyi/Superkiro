const assert = require('node:assert/strict');
const fs = require('node:fs'), vm = require('node:vm'), ts = require('typescript');
const exportsObject = {};
vm.runInNewContext(ts.transpileModule(fs.readFileSync(require('node:path').join(__dirname, '../src/runtimeSettings.ts'), 'utf8'), {compilerOptions:{module:ts.ModuleKind.CommonJS}}).outputText, {exports:exportsObject, TextEncoder});
const {runtimeError} = exportsObject;
const profile = {headers_secs:15, attempt_secs:65, total_secs:150, commit_secs:45, started_secs:600, idle_secs:90};
const valid = () => ({standard:{...profile}, reasoning:{...profile, headers_secs:45, attempt_secs:90, idle_secs:180}, claude:{...profile, headers_secs:90, attempt_secs:180, total_secs:300, idle_secs:180}, openai_reasoning_idle_secs:300, keepalive_secs:20});
assert.equal(runtimeError(valid()), '');
for (const change of [s=>s.keepalive_secs=0,s=>s.keepalive_secs=26,s=>s.claude.headers_secs=181,s=>s.claude.total_secs=100,s=>s.reasoning.started_secs=200,s=>s.claude.idle_secs=601,s=>s.standard.commit_secs=46,s=>s.standard.headers_secs=1.5,s=>s.openai_reasoning_idle_secs=601]) {
 const s=valid();change(s);assert.ok(runtimeError(s));
}


assert.equal(exportsObject.runtimeReasonError('汉'.repeat(341)), '');
assert.ok(exportsObject.runtimeReasonError('汉'.repeat(342)));
assert.ok(exportsObject.runtimeReasonError('bad\nreason'));

// Match the transmitted (trimmed) UTF-8 byte limit, not JS character count.
for (const reason of ['a'.repeat(1024), '😀'.repeat(256), '  valid reason  ', ' ' + '汉'.repeat(341) + ' ']) {
 assert.equal(exportsObject.runtimeReasonError(reason), '');
}
for (const reason of ['', '   ', 'a'.repeat(1025), '😀'.repeat(257)]) {
 assert.ok(exportsObject.runtimeReasonError(reason));
}
for (let code = 0; code <= 0x9f; code++) {
 if (code <= 0x1f || code >= 0x7f) assert.ok(exportsObject.runtimeReasonError('valid' + String.fromCharCode(code)), 'reject control ' + code);
}
console.log('Runtime settings and reason validation passed');
