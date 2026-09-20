"""Runs on the server against two isolated loopback-only recovery containers."""
import http.client
import ipaddress
import json
import subprocess
import sys
import time
from pathlib import Path
from urllib.request import Request, build_opener, ProxyHandler, HTTPRedirectHandler
from urllib.error import HTTPError


def cmd(*args):
    p=subprocess.run(args,stdout=subprocess.PIPE,stderr=subprocess.PIPE,check=False,timeout=35)
    if p.returncode: raise RuntimeError('Recovery container operation failed')
    return p.stdout.decode().strip()


def env_values(path):
    return {k:v.strip().strip("'\"") for l in Path(path).read_text().splitlines() if '=' in l and not l.startswith('#') for k,v in [l.split('=',1)]}


def main():
    dest=Path(sys.argv[1]); report=json.loads((dest/'release-state.json').read_text())
    if str(dest) != '/opt/kiro-byok/releases/'+report['release']:raise RuntimeError('Invalid path')
    backup=Path(report['backup']); web=json.loads(Path('/etc/kiro-byok/admin-access.json').read_text())
    origin='https://kiro.rent'; base='http://127.0.0.1:19821'
    class NoRedirect(HTTPRedirectHandler):
        def redirect_request(self,*args,**kwargs):raise RuntimeError('Redirect forbidden')
    opener=build_opener(ProxyHandler({}),NoRedirect())
    state={}
    def req(path,body=None,headers=None,expected=200):
        h={'Content-Type':'application/json',**state,**(headers or {})}
        r=Request(base+path,data=None if body is None else json.dumps(body).encode(),headers=h)
        try: response=opener.open(r,timeout=15)
        except HTTPError as e: response=e
        if response.status!=expected: raise RuntimeError('Recovery HTTP check failed: '+path+' '+str(response.status))
        data=response.read()
        return (json.loads(data) if data else {},response.headers)
    def login(browser,env):
        state.clear()
        if browser:
            state['Origin']=origin
            _,h=req('/api/v1/admin/session',{'username':web['username'],'password':web['password']})
            state['Cookie']=h['Set-Cookie'].split(';',1)[0]
            d,_=req('/api/v1/admin/session');state['x-csrf-token']=d['csrfToken']
        else:
            d,_=req('/api/v1/admin/session',{}, {'x-admin-key':env['ADMIN_KEY']})
            state['Authorization']='Bearer '+d['accessToken']
    def cards():
        result=[]
        for offset in range(0,100000,500):
            d,_=req('/api/v1/admin/cards?offset='+str(offset)+'&limit=500');result+=d['cards']
            if len(d['cards'])<500:return sorted(result,key=lambda c:c['id'])
        raise RuntimeError('Card pagination exceeded limit')
    def start(name,image,envfile,data):
        nonlocal base
        cmd('docker','run','-d','--name',name,'--security-opt','no-new-privileges:true','--cap-drop','ALL',
            '--network','superkiro-recovery-'+report['release'].lower(),'--memory','1g','--cpus','1','--env-file',str(envfile),'-v',str(data)+':/app/data',image)
        networks=json.loads(cmd('docker','inspect',name))[0]['NetworkSettings']['Networks']
        address=ipaddress.ip_address(networks['superkiro-recovery-'+report['release'].lower()]['IPAddress'])
        if address.version!=4 or not address.is_private:raise RuntimeError('Invalid isolated container address')
        base='http://'+str(address)+':19820'
        for _ in range(30):
            try:
                req('/healthz');return
            except Exception:time.sleep(1)
        raise RuntimeError('Recovery health failed')
    snapshots=[]; original_codes={}
    oldenv=json.loads(cmd('docker','compose','-p','deploy','-f',report['previous_release']+'/deploy/docker-compose.ip.yml','config','--format','json'))['services']['gateway']['environment']
    newenv=json.loads(cmd('docker','compose','-p','deploy','-f',str(dest/'auth-migration/compose.verify.yml'),'config','--format','json'))['services']['gateway']['environment']
    oldenv={k:str(v).replace('$$','$') for k,v in oldenv.items()}
    newenv={k:str(v).replace('$$','$') for k,v in newenv.items()}
    running=dict(item.split('=',1) for item in json.loads(cmd('docker','inspect','kiro-gateway'))[0]['Config']['Env'])
    if any(running.get(k)!=v for k,v in oldenv.items()):raise RuntimeError('Resolved old environment differs from deployed container')
    if any(newenv.get(k)!=v for k,v in oldenv.items() if k not in {'ADMIN_BROWSER_LOGIN','ADMIN_ORIGIN','ADMIN_PASSWORD_HASH'}):raise RuntimeError('Effective environment changed')
    for label,image,source in [('old',report['previous_image_id'],backup/'configuration/gateway.env'),('new',report['image_id'],dest/'auth-migration/gateway.env')]:
        name='superkiro-recovery-'+label+'-'+report['release'].lower()
        work=dest/('recovery-'+label);work.mkdir(mode=0o700)
        cmd('cp','-a',str(backup/'data'),str(work/'data'))
        env=oldenv if label=='old' else newenv;envfile=work/'runtime.env'
        envfile.write_text(''.join(k+'='+v+'\n' for k,v in env.items()));envfile.chmod(0o600)
        try:
            start(name,image,envfile,work/'data');login(label=='new',env)
            existing=cards();snapshots.append(existing)
            if label=='old':
                for card in existing:
                    if card.get('codeRecoverable'):
                        value,_=req('/api/v1/admin/cards/reveal',{'cardId':card['id']});original_codes[card['id']]=value['rawCode']
            if label=='new':
                old=snapshots[0]
                fields=['id','status','creditTotal','creditUsed','availableCredits','boundDevices','maxDevices','activatedAt','validUntil','groupId']
                if [{k:c[k] for k in fields} for c in old]!=[{k:c[k] for k in fields} for c in existing]:raise RuntimeError('Restored card balances/state differ')
                if [bool(c.get('codeRecoverable')) for c in old]!=[bool(c.get('codeRecoverable')) for c in existing]:raise RuntimeError('Card recoverability changed')
                for card in existing:
                    if card.get('codeRecoverable'):
                        value,_=req('/api/v1/admin/cards/reveal',{'cardId':card['id']})
                        if value.get('rawCode')!=original_codes[card['id']]:raise RuntimeError('Encrypted card reveal changed')
                d,_=req('/api/v1/admin/cards/batch',{'count':1,'groupId':'group-pro-plus','templateId':'tier-1000','note':'isolated restore verification'})
                card=d['cards'][0]
                card_id=card.get('cardId',card.get('card_id',card.get('id')))
                raw=card.get('rawCode',card.get('raw_code'))
                if not card_id or not raw:raise RuntimeError('Unexpected generated card schema')
                req('/api/v1/admin/snapshot/sync',{})
                cmd('docker','restart','-t','30',name)
                for _ in range(30):
                    try:req('/healthz');break
                    except Exception:time.sleep(1)
                login(True,env);reveal,_=req('/api/v1/admin/cards/reveal',{'cardId':card_id})
                if reveal['rawCode']!=raw:raise RuntimeError('Card ciphertext persistence failed')
                for card_id,original in original_codes.items():
                    value,_=req('/api/v1/admin/cards/reveal',{'cardId':card_id})
                    if value.get('rawCode')!=original:raise RuntimeError('Existing ciphertext restart recovery failed')
        finally:
            cmd('docker','rm','-f',name)
    print(json.dumps({'restoredCards':len(snapshots[0]),'balancesAndBindingsPreserved':True,'encryptedCardRestartReveal':True}))

if __name__=='__main__':main()
