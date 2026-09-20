"""Sanitized production inspection and release checks; secrets stay in memory."""
import json
import requests
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
BASE='https://kiro.rent'

def connection(ssh):
    with ssh.open_sftp() as f:
        env=dict(l.split('=',1) for l in f.open('/etc/kiro-byok/gateway.env').read().decode().splitlines() if '=' in l and not l.startswith('#'))
        credentials=json.loads(f.open('/etc/kiro-byok/admin-access.json').read())
    s=requests.Session(); s.trust_env=False
    try:
        r=s.post(BASE+'/api/v1/admin/session',json={'username':credentials.get('username','admin'),'password':credentials['password']},headers={'Origin':BASE},timeout=30,allow_redirects=False)
        r.raise_for_status()
        r=s.get(BASE+'/api/v1/admin/session',timeout=30,allow_redirects=False)
        r.raise_for_status()
        csrf=r.json()['csrfToken']
        if not csrf: raise RuntimeError('Admin session missing CSRF token')
    except Exception:
        s.close()
        raise
    admin={'Origin':BASE,'x-csrf-token':csrf}
    def req(path,payload=None,headers=None,method=None):
        r=s.request(method or ('GET' if payload is None else 'POST'),BASE+path,json=payload,headers=admin if headers is None else headers,timeout=(15,180),allow_redirects=False)
        if r.status_code != 200: raise RuntimeError(f'{path}: HTTP {r.status_code}')
        return r
    return env,s,admin,req

def inspect(ssh):
    env,s,admin,req=connection(ssh)
    try:
        providers=req('/api/v1/admin/providers').json()
        config=req('/api/v1/admin/commercial-config').json()
        print('PROVIDERS',json.dumps([{k:p.get(k) for k in ['id','name','provider_type','base_url','enabled']} for p in providers['providers']]))
        print('KEYS',json.dumps([{k:p.get(k) for k in ['id','provider_id','enabled','allowed_models']} for p in providers['keys']]))
        print('COMMERCIAL_CONFIG',json.dumps(config,ensure_ascii=False)[:18000])
        result=[]
        for key in providers['keys']:
            if key.get('enabled'):
                d=req('/api/v1/admin/providers/keys/discover',{'provider_id':key['provider_id'],'key_id':key['id']}).json()
                result.append({'provider':key['provider_id'],'key_id':key['id'],'models':d.get('models'),'published':d.get('published'),'success':d.get('success')})
        (ROOT/'.acceptance/domain-upstream-discovery.json').write_text(json.dumps(result,indent=2),encoding='utf-8')
        print('DISCOVERY',json.dumps(result,ensure_ascii=False)[:12000])
    finally: s.close()

def acceptance(ssh):
    """Real TLS, disposable entitlement, real upstream calls; never logs secrets."""
    import uuid
    from test_single_server_smoke import decode_frames
    env,s,admin,req=connection(ssh); checks=[];card=None
    def check(name,condition=True,**details):
        if not condition: raise AssertionError(name)
        checks.append(dict(check=name,passed=True,**details));print('PASS '+name,flush=True)
    def status(path,payload=None,headers=None):
        return s.request('GET' if payload is None else 'POST',BASE+path,json=payload,headers=headers,timeout=30).status_code
    try:
        for proxy in [None,'http://127.0.0.1:7897']:
            with requests.Session() as client:
                client.trust_env=False
                if proxy:client.proxies={'https':proxy,'http':proxy}
                for path in ['/healthz','/','/device','/docs','/downloads/releases.json']:
                    r=client.get(BASE+path,timeout=30);check(('proxy' if proxy else 'direct')+' TLS '+path,r.status_code==200)
        with requests.Session() as anonymous:
            anonymous.trust_env=False
            page=anonymous.get(BASE+'/admin/',timeout=30,allow_redirects=False)
            api=anonymous.get(BASE+'/api/v1/admin/cards',timeout=30,allow_redirects=False)
            check('anonymous login page available; admin API protected',page.status_code==200 and api.status_code==401 and 'WWW-Authenticate' not in page.headers and 'WWW-Authenticate' not in api.headers)
        check('root model RPC not intercepted by website',status('/',{}, {'x-amz-target':'KiroRuntimeService.GenerateAssistantResponse'})==401)
        card=req('/api/v1/admin/cards/batch',{'count':1,'templateId':'tier-1000','groupId':'group-pro-plus','note':'domain acceptance disposable'}).json()['cards'][0]
        raw=card['rawCode']; q=lambda:req('/api/v1/portal/query',{'card':raw}).json()
        initial=q();check('read-only verification preserves unused 1000 PRO card',initial['remainingPoints']==1000 and initial['virtualPlanName']=='PRO' and initial['activatedAt'] is None and not initial['boundDevices'] and initial['maxDevices']==1)
        device='domain-e2e-'+uuid.uuid4().hex
        tokens=req('/oauth/token',{'card_key':raw,'device_id':device}).json();auth={'Authorization':'Bearer '+tokens['accessToken']}
        check('one card refuses second concurrent device',status('/oauth/token',{'card_key':raw,'device_id':device+'-second'}) in (400,401,403,409))
        active=q();check('activation uses 30 day issued duration',active['validUntil']-active['activatedAt']==30*86400)
        usage=req('/getUsageLimits',headers=auth).json();check('Kiro subscription displays PRO',usage['subscriptionInfo']['subscriptionTitle']=='PRO')
        models=req('/List-Available-Models',headers=auth).json();ids=[m['modelId'] for m in models['models']]
        check('published models and default align',len(ids)==6 and models['defaultModel']['modelId']=='claude-sonnet-4-6',models=ids)
        for model in ids:
            before=q()['remainingCredits'];invocation=uuid.uuid4().hex
            headers={**auth,'x-amz-target':'KiroRuntimeService.GenerateAssistantResponse','amz-sdk-invocation-id':invocation}
            payload={'conversationState':{'conversationId':'domain-'+uuid.uuid4().hex,'history':[],'currentMessage':{'userInputMessage':{'content':'Reply only with OK.','modelId':model}}}}
            response=req('/',payload,headers);frames=decode_frames(response.content)
            check(model+' real upstream reply','ok' in ''.join(f.get('content','') for f in frames).lower() and any(f.get('stopReason')=='end_turn' for f in frames))
            after=q()['remainingCredits'];check(model+' debits credits',after<before,microcredits=before-after)
            req('/',payload,headers);check(model+' replay does not charge twice',q()['remainingCredits']==after)
        before=q();challenge=req('/api/v1/portal/challenge',{'action':'unbind'}).json()['challengeToken']
        unbind={'card':raw,'device':device,'challenge_token':challenge}
        req('/api/v1/portal/unbind',unbind)
        after=q();check('unbind preserves credit and expiry',not after['boundDevices'] and after['remainingCredits']==before['remainingCredits'] and after['validUntil']==before['validUntil'])
        check('unbind invalidates old access token',status('/List-Available-Models',headers=auth)==401)
        check('unbind invalidates old refresh token',status('/refreshToken',{'refreshToken':tokens['refreshToken']})==401)
        check('challenge cannot be replayed',status('/api/v1/portal/unbind',unbind) in (400,403))
        req('/oauth/token',{'card_key':raw,'device_id':device+'-replacement'})
        check('replacement device can bind',q()['boundDevices']==[device+'-replacement'])
    finally:
        try:
            if card:
                req('/api/v1/admin/cards/status',{'cardId':card['cardId'],'action':'ban','reason':'domain acceptance completed'})
                check('disposable card banned')
        finally:
            (ROOT/'.acceptance/domain-e2e-results.json').write_text(json.dumps(checks,indent=2),encoding='utf-8');s.close()

def issue_user_card(ssh):
    """Issue once, store privately, and only perform read-only entitlement query."""
    env,s,admin,req=connection(ssh)
    path=ROOT/'.acceptance/private/user-test-card-20260919.json';path.parent.mkdir(exist_ok=True)
    try:
        if path.exists(): card=json.loads(path.read_text(encoding='utf-8'))
        else:
            card=req('/api/v1/admin/cards/batch',{'count':1,'templateId':'tier-1000','groupId':'group-pro-plus','note':'Owner final acceptance 2026-09-19 unused PRO'}).json()['cards'][0]
            path.write_text(json.dumps(card,indent=2),encoding='utf-8')
        q=req('/api/v1/portal/query',{'card':card['rawCode']}).json()
        assert q['remainingPoints']==1000 and not q['boundDevices'] and q['activatedAt'] is None and q['virtualPlanName']=='PRO'
        print('PASS user card unused / PRO / 1000 points / 1 device; private recovery file saved')
    finally:s.close()
