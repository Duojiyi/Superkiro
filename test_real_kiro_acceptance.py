"""Readiness probe and isolated real Kiro launch. Prompts for SSH password.
prepare issues one test card; cleanup bans it and removes the test token.
Does not modify the installed extension or the user's normal profile.
"""
import argparse, getpass, json, os, subprocess, sys, uuid
from pathlib import Path
import requests
from test_deployed_server import ROOT, HOST, connect

BASE = ROOT / '.acceptance' / 'kiro'

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=['probe','prepare','cleanup'])
    parser.add_argument('--stdin-config',action='store_true')
    args=parser.parse_args()
    mode=args.mode
    config=json.load(sys.stdin) if args.stdin_config else {'password':getpass.getpass('SSH password: ')}
    ssh = connect(config['password'], use_proxy=False)
    sftp = ssh.open_sftp()
    env = dict(line.split('=', 1) for line in sftp.open('/etc/kiro-byok/gateway.env').read().decode().splitlines() if '=' in line)
    sftp.close()
    s = requests.Session(); s.trust_env = False; s.verify = str(ROOT/'deploy/server-ca.pem')
    def req(path, **kw):
        r=s.request(kw.pop('method','GET'),'https://'+HOST+path,timeout=(15,45),**kw)
        return r
    login=req('/api/v1/admin/session',method='POST',json={},headers={'x-admin-key':env['ADMIN_KEY']})
    login.raise_for_status()
    admin={'Authorization':'Bearer '+login.json()['accessToken']}
    manifest=BASE/'manifest.json'
    token_path=BASE/'home/.aws/sso/cache/kiro-auth-token.json'
    issued_id = None
    try:
        if mode == 'cleanup':
            if manifest.exists():
                card_id=json.loads(manifest.read_text())['card_id']
                r=req('/api/v1/admin/cards/status',method='POST',headers=admin,json={'cardId':card_id,'action':'ban','reason':'isolated IDE acceptance cleanup'})
                r.raise_for_status()
                token_path.unlink(missing_ok=True)
                manifest.unlink(missing_ok=True)
                print('Test card banned; isolated token removed')
            return
        if manifest.exists():
            raise RuntimeError('Prior acceptance card exists; cleanup before another prepare')
        BASE.mkdir(parents=True,exist_ok=True)
        subprocess.run(['icacls',str(BASE),'/inheritance:r','/grant:r',subprocess.check_output(['whoami'], text=True).strip()+':(OI)(CI)F','SYSTEM:(OI)(CI)F'],check=True,stdout=subprocess.DEVNULL)
        card=req('/api/v1/cards/pull',method='POST',headers={'X-Card-Platform-Key':env['CARD_PLATFORM_KEY']},json={'order_id':'ide-acceptance-'+uuid.uuid4().hex,'count':1})
        card.raise_for_status();card=card.json()['cards'][0]
        issued_id=card['card_id']
        manifest.write_text(json.dumps({'card_id':card['card_id'],'host':HOST}),encoding='utf-8')
        auth=req('/oauth/token',method='POST',json={'card_key':card['raw_code'],'device_id':'isolated-real-kiro-acceptance'})
        auth.raise_for_status();auth=auth.json()
        token_path.parent.mkdir(parents=True,exist_ok=True)
        token_path.write_text(json.dumps({k:auth[k] for k in ['accessToken','refreshToken','profileArn','expiresAt','authMethod','provider']}),encoding='utf-8')
        headers={'Authorization':'Bearer '+auth['accessToken']}
        results={'host':HOST,'admin_feature_routes':{},'kiro':'not_accepted: real GUI workflow pending',
            'overall':'blocked','totp':'not_accepted: session issued without a second factor',
            'rls':'not_accepted: production uses in-process encrypted file ledger'}
        for path in ['/api/v1/admin/groups','/api/v1/admin/pricing','/api/v1/admin/totp']:
            results['admin_feature_routes'][path]=req(path,headers=admin).status_code
        r=req('/GenerateCompletions',method='POST',headers=headers,json={'fileContext':{'filename':'sum.py','leftFileContent':'def add(a, b):\n    return ','rightFileContent':'\n'}})
        results['completion']={'status':r.status_code,'body':r.json()}
        r=req('/mcp',method='POST',headers=headers,json={'jsonrpc':'2.0','id':1,'method':'tools/call','params':{'name':'web_search','arguments':{'query':'Rust programming language official documentation'}}})
        results['search']={'status':r.status_code,'body':r.json()}
        search_body=results['search']['body']
        results['search']['synthetic_detected']='processed by Kiro BYOK Gateway.' in json.dumps(search_body)
        results['search']['accepted']=False  # Real search evidence and failure handling both required.
        results['completion']['accepted']=results['completion']['status'] == 200 and bool(results['completion']['body'].get('completions'))
        results['blocking_features']=['dynamic_groups','pricing_publication','totp','database_rls','external_search','dedicated_completion','real_kiro_workflow']
        (ROOT/'deployment-readiness-results.json').write_text(json.dumps(results,ensure_ascii=False,indent=2),encoding='utf-8')
        print(json.dumps(results,ensure_ascii=False,indent=2),flush=True)
        if mode == 'probe':
            return 2  # An incomplete product must not yield a successful acceptance exit code.
        profile=BASE/'profile';(profile/'User').mkdir(parents=True,exist_ok=True)
        (profile/'User/settings.json').write_text(json.dumps({'kiroAgent.enableTabAutocomplete':True,'update.mode':'none','workbench.startupEditor':'none'}),encoding='utf-8')
        work=BASE/'workspace';work.mkdir(exist_ok=True)
        (work/'sum.py').write_text('def add(a, b):\n    return a + b\n',encoding='utf-8')
        child_env=dict(os.environ)
        child_env.update(USERPROFILE=str(BASE/'home'),HOME=str(BASE/'home'),APPDATA=str(BASE/'roaming'),LOCALAPPDATA=str(BASE/'local'),KIRO_GATEWAY_URL='https://'+HOST,AWS_ENDPOINT_URL='https://'+HOST,KIRO_AUTH_PORTAL_URL='https://'+HOST,NODE_EXTRA_CA_CERTS=str(ROOT/'deploy/server-ca.pem'))
        for key in ['ELECTRON_RUN_AS_NODE','NODE_TLS_REJECT_UNAUTHORIZED']:child_env.pop(key,None)
        exe=Path(os.environ.get('LOCALAPPDATA', str(Path.home()/'AppData/Local')))/'Programs/Kiro/Kiro.exe'
        p=subprocess.Popen([str(exe),'--new-window','--user-data-dir',str(profile),'--extensions-dir',str(BASE/'extensions'),str(work)],env=child_env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        print('Isolated Kiro launched; PID='+str(p.pid),flush=True)
    except Exception:
        if issued_id:
            ban=req('/api/v1/admin/cards/status',method='POST',headers=admin,json={'cardId':issued_id,'action':'ban','reason':'failed IDE acceptance preparation'})
            ban.raise_for_status()
            token_path.unlink(missing_ok=True)
            manifest.unlink(missing_ok=True)
        raise
    finally:
        if mode == 'probe' and issued_id:
            ban=req('/api/v1/admin/cards/status',method='POST',headers=admin,json={'cardId':issued_id,'action':'ban','reason':'readiness probe complete'})
            ban.raise_for_status()
            token_path.unlink(missing_ok=True)
            manifest.unlink(missing_ok=True)
        req('/api/v1/admin/session/revoke',method='POST',json={},headers=admin).raise_for_status()
        ssh.close()

if __name__=='__main__':sys.exit(main() or 0)
