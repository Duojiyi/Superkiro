"""Publish only hashed admin assets; switch index last and retain rollback files.
Credentials are accepted on stdin, never persisted or printed.
"""
import hashlib
import json
import re
import shlex
import sys
from datetime import datetime, timezone
from pathlib import Path
import requests
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from deploy.release_candidate import pinned_connection, run, deployment_lock, write_remote, ROOT

def main():
    config=json.load(sys.stdin)
    acceptance=ROOT/'.acceptance'
    acceptance.mkdir(exist_ok=True)
    probe=acceptance/'.admin-publish-write-check'
    probe.write_text('ready',encoding='utf8'); probe.unlink()
    dist=ROOT/'apps/admin-ui/dist'
    index=(dist/'index.html').read_bytes()
    names=re.findall(r'(?:src|href)="/admin/(assets/[^"?#]+)"',index.decode())
    if not names or any(not re.fullmatch(r'assets/[A-Za-z0-9_.-]+',n) for n in names):
        raise RuntimeError('Unexpected admin asset paths')
    files={n:(dist/n).read_bytes() for n in names}
    ssh=pinned_connection(config)
    report={'scope':'admin-static-only','status':'pending'}
    try:
        with deployment_lock(ssh):
            mounts=json.loads(run(ssh,"docker inspect kiro-caddy --format '{{json .Mounts}}'"))
            candidates=[m['Source'] for m in mounts if m['Destination']=='/app/admin-ui']
            if len(candidates)!=1 or not re.fullmatch(r'/opt/kiro-byok/releases/[A-Za-z0-9_-]+/apps/admin-ui/dist',candidates[0]):
                raise RuntimeError('Unexpected admin mount')
            dest=candidates[0]
            with ssh.open_sftp() as sftp:
                with sftp.open(dest+'/index.html','rb') as f: previous=f.read()
            stamp=datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')
            backup='/opt/kiro-byok/frontend-backups/'+stamp
            run(ssh,'mkdir -p '+shlex.quote(backup))
            write_remote(ssh,backup+'/index.html',previous)
            for name,data in files.items():
                write_remote(ssh,dest+'/'+name,data)
                run(ssh,'chmod 644 '+shlex.quote(dest+'/'+name))
                actual=run(ssh,'sha256sum '+shlex.quote(dest+'/'+name)).split()[0]
                if actual!=hashlib.sha256(data).hexdigest(): raise RuntimeError('Remote asset hash mismatch')
            try:
                write_remote(ssh,dest+'/index.html',index)
                run(ssh,'chmod 644 '+shlex.quote(dest+'/index.html'))
                session=requests.Session(); session.trust_env=False
                response=session.get('https://kiro.rent/admin/',timeout=30)
                response.raise_for_status()
                if response.content!=index: raise RuntimeError('Public admin index mismatch')
                for name,data in files.items():
                    response=session.get('https://kiro.rent/admin/'+name,timeout=30)
                    response.raise_for_status()
                    if hashlib.sha256(response.content).digest()!=hashlib.sha256(data).digest():
                        raise RuntimeError('Public admin asset mismatch')
                response=session.get('https://kiro.rent/api/v1/admin/cards',timeout=30)
                if response.status_code not in (401,403):raise RuntimeError('Anonymous admin cards were not rejected')
                response=session.get('https://kiro.rent/healthz',timeout=30);response.raise_for_status()
            except Exception:
                write_remote(ssh,dest+'/index.html',previous)
                run(ssh,'chmod 644 '+shlex.quote(dest+'/index.html'))
                raise RuntimeError('Verification failed; prior admin index restored') from None
            report.update(status='published',published_at=stamp,mount=dest,rollback_index=backup+'/index.html',assets={n:hashlib.sha256(d).hexdigest() for n,d in files.items()},index_sha256=hashlib.sha256(index).hexdigest(),checks=['public index and asset hashes match','anonymous cards rejected','gateway health OK'])
            # Public verification is the commit point; recording failure must not
            # misreport an already-live release as a failed deployment.
            warnings=[]
            try: write_remote(ssh,backup+'/report.json',json.dumps(report).encode())
            except Exception: warnings.append('Remote report write failed; release is live')
            report['warnings']=warnings
            try: (acceptance/'admin-bulk-publish.json').write_text(json.dumps(report,indent=2),encoding='utf8')
            except OSError: warnings.append('Local report write failed; release is live')
            print(json.dumps(report),flush=True)
    finally:ssh.close()

if __name__=='__main__':main()
