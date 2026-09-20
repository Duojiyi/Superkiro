"""Complete a staged frontend release with its audited management API contracts."""
import difflib
import json
import shlex
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from deploy import release_candidate as rc
from deploy.release_announcements import read

FILES = [
    'crates/gateway/src/facade/admin.rs',
    'crates/gateway/src/facade/admin_login.rs',
    'crates/gateway/src/facade/provider_import.rs',
    'crates/gateway/tests/admin_api_test.rs',
    'crates/gateway/tests/admin_browser_login_test.rs',
    'crates/gateway/tests/admin_card_recovery_test.rs',
    'crates/gateway/tests/provider_import_test.rs',
]
ENGINE = 'crates/billing/src/engine.rs'
TEST = 'crates/gateway/tests/frontend_cloud_contract_test.rs'

def between(text, start, end):
    if text.count(start) != 1 or text.count(end) != 1:
        raise RuntimeError('Source anchor mismatch')
    return text[text.index(start):text.index(end)]

def main():
    ssh = rc.pinned_connection(json.load(sys.stdin))
    try:
        with rc.deployment_lock(ssh):
            report = json.loads((rc.ROOT/'deployment-candidate-results.json').read_text(encoding='utf8'))
            if report['status'] != 'staging':
                raise RuntimeError('Candidate is not staging')
            dest = rc.release_path(rc.BASE+'/releases/'+report['release'])
            old = rc.release_path(report['previous_release'])
            paths = rc.SOURCE_PATHS + ['deploy/docker-compose.ip.yml', 'deploy/Caddyfile.ip']
            if rc.run(ssh, 'readlink -f /opt/kiro-byok/current') != old or rc.run(ssh, f'cat {dest}/build.exit') != '0':
                raise RuntimeError('Baseline build/live release changed')
            if rc.tree_digest(ssh,dest,paths) != report['candidate_sha256']:
                raise RuntimeError('Staged source changed')
            # Invalidate the old binary before changing any candidate source or manifest.
            rc.run(ssh, f"printf 'pending' > {dest}/build.exit")
            for path in FILES:
                rc.write_remote(ssh,dest+'/'+path,(rc.ROOT/path).read_bytes())
            original=read(ssh,dest+'/'+ENGINE).replace('\r','')
            local=(rc.ROOT/ENGINE).read_text(encoding='utf8')
            start='    pub fn upsert_providers_checked<I, K>'
            end='    pub fn list_providers(&self)'
            patched=original.replace(between(original,start,end),between(local,start,end))
            anchor='if existing.card_id == card_id && existing.credits_charged == delta_micro_credits {'
            if patched.count(anchor)!=1:raise RuntimeError('Idempotency anchor mismatch')
            patched=patched.replace(anchor,'if existing.card_id == card_id && existing.credits_charged == delta_micro_credits && existing.operator_id.as_deref() == Some(op) && existing.reason.as_deref() == Some(res) {')
            rc.write_remote(ssh,dest+'/'+ENGINE,patched.encode())
            source=(rc.ROOT/'crates/gateway/tests/cloud_audit_regression_test.rs').read_text(encoding='utf8')
            selected=source[:source.index('#[test]')]
            selected+=between(source,'fn adjustment(', '#[tokio::test]\nasync fn cooldown_starts')
            selected+=between(source,'#[tokio::test]\nasync fn financials_reports', '#[tokio::test]\nasync fn usage_reports')
            rc.write_remote(ssh,dest+'/'+TEST,selected.encode())
            extra=FILES+[ENGINE,TEST]
            report['scope']=sorted(set(report['scope']+extra))
            # Independently enumerate the final delta against the still-live source.
            diff_script='''import hashlib,json,pathlib,sys
old,new=map(pathlib.Path,sys.argv[1:3]);paths=json.loads(sys.argv[3])
def files(root):
 result={}
 for item in paths:
  p=root/item
  for f in ([p] if p.is_file() else p.rglob('*')):
   if f.is_file():result[str(f.relative_to(root))]=hashlib.sha256(f.read_bytes()).hexdigest()
 return result
a,b=files(old),files(new)
assert set(a).issubset(b)
print(json.dumps(sorted(k for k in b if a.get(k)!=b[k])))'''
            changed=json.loads(rc.run(ssh,'python3 -c '+shlex.quote(diff_script)+' '+shlex.join([old,dest,json.dumps(paths)])))
            if not set(changed).issubset(report['scope']):raise RuntimeError('Out-of-scope source change')
            report['scope']=report['verified_changed_paths']=changed
            report['candidate_sha256']=rc.tree_digest(ssh,dest,paths)
            rc.save_report(ssh,report)
            # Keep the prior successful build layer as cache, then compile the extra contracts.
            build=read(ssh,dest+'/Dockerfile.announcements')
            index=build.index('FROM debian:')
            supplement=''.join(f'COPY {p} {p}\n' for p in extra)
            supplement+='RUN CARGO_BUILD_JOBS=1 cargo test --locked -p gateway --lib admin_login && CARGO_BUILD_JOBS=1 cargo test --locked -p gateway --test admin_api_test --test admin_browser_login_test --test admin_card_recovery_test --test provider_import_test --test frontend_cloud_contract_test --test announcements_test && CARGO_BUILD_JOBS=1 cargo build --locked --release -p gateway --bin gateway\n'
            rc.write_remote(ssh,dest+'/Dockerfile.announcements',(build[:index]+supplement+build[index:]).encode())
            rc.run(ssh,f"printf 'pending' > {dest}/build.exit\nsh {dest}/limited-build.sh",timeout=7200)
            report['management_contract_tests']=rc.run(ssh,f"grep 'test result:' {dest}/build.log")
            rc.save_report(ssh,report)
            print('READY management contracts: '+report['release'],flush=True)
    finally: ssh.close()

if __name__=='__main__': main()
