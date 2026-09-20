"""Live release acceptance; credentials/cards stay in memory and disposable card is banned."""
import json
import sys
import uuid
from pathlib import Path
sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
import requests
from test_deployed_server import connect,ROOT
from test_single_server_smoke import decode_frames


def main():
    ssh=connect(json.load(sys.stdin)['password'],False)
    admin=requests.Session();admin.trust_env=False
    public=requests.Session();public.trust_env=False
    base='https://kiro.rent'; card=None; checks=[]
    def check(name,condition):
        if not condition:raise RuntimeError(name)
        checks.append(name);print('PASS '+name,flush=True)
    def req(session,path,data=None,headers=None,expected=200):
        r=session.request('GET' if data is None else 'POST',base+path,json=data,headers=headers,timeout=(15,180),allow_redirects=False)
        if r.status_code!=expected:raise RuntimeError(path+' status '+str(r.status_code))
        return r
    try:
        with ssh.open_sftp() as f:
            web=json.loads(f.open('/etc/kiro-byok/admin-access.json').read())
            env=dict(l.split('=',1) for l in f.open('/etc/kiro-byok/gateway.env').read().decode().splitlines() if '=' in l)
        for path in ['/','/docs','/device','/healthz','/admin/']:
            r=req(public,path);check(path+' accessible without Basic challenge','WWW-Authenticate' not in r.headers)
        req(public,'/api/v1/admin/stats',expected=401)
        denied=public.post(base+'/api/v1/admin/session',json={},headers={'Origin':base,'x-admin-key':env['ADMIN_KEY']},timeout=20)
        check('Legacy raw administrator key rejected',denied.status_code in (400,401))
        admin.headers['Origin']=base
        r=req(admin,'/api/v1/admin/session',{'username':web['username'],'password':web['password']})
        cookie=r.headers.get('Set-Cookie','').lower()
        check('Secure HttpOnly Strict admin cookie',all(x in cookie for x in ['secure','httponly','samesite=strict']))
        csrf=req(admin,'/api/v1/admin/session').json()['csrfToken']
        req(admin,'/api/v1/admin/cards/batch',{'count':1},expected=403)
        admin.headers['x-csrf-token']=csrf
        req(admin,'/api/v1/admin/stats')
        r=req(admin,'/api/v1/admin/cards/batch',{'count':1,'templateId':'tier-1000','groupId':'group-pro-plus','note':'native release smoke disposable'})
        card=r.json()['cards'][0]
        check('1000 point single-device PRO card issued',card['creditTotal']==1000000000 and card['maxDevices']==1 and card['virtualPlanName']=='PRO')
        revealed=req(admin,'/api/v1/admin/cards/reveal',{'cardId':card['cardId']})
        check('Admin can reveal exact card with no-store',revealed.json()['rawCode']==card['rawCode'] and 'no-store' in revealed.headers.get('Cache-Control',''))
        portal=req(public,'/api/v1/portal/query',{'card':card['rawCode']}).json()
        check('Query card does not bind device',not portal['boundDevices'])
        token=req(public,'/oauth/token',{'card_key':card['rawCode'],'device_id':'release-smoke-'+uuid.uuid4().hex}).json()['accessToken']
        auth={'Authorization':'Bearer '+token}
        denied=public.post(base+'/oauth/token',json={'card_key':card['rawCode'],'device_id':'other-'+uuid.uuid4().hex},timeout=30)
        check('Second device binding rejected',denied.status_code in (400,401,403,409))
        models=req(public,'/List-Available-Models',headers=auth).json()['models']
        check('Multiple gateway models listed',len(models)>=6)
        for m in models:
            model=m['modelId'];before=req(public,'/getUsageLimits',headers=auth).json()['usageBreakdownList'][0]['currentUsageWithPrecision']
            headers={**auth,'x-amz-target':'KiroRuntimeService.GenerateAssistantResponse','amz-sdk-invocation-id':uuid.uuid4().hex}
            payload={'conversationState':{'conversationId':'release-smoke-'+uuid.uuid4().hex,'history':[],'currentMessage':{'userInputMessage':{'content':'Reply only with OK.','modelId':model}}}}
            frames=decode_frames(req(public,'/',payload,headers).content)
            check(model+' reply complete','ok' in ''.join(x.get('content','') for x in frames).lower() and any(x.get('stopReason')=='end_turn' for x in frames))
            after=req(public,'/getUsageLimits',headers=auth).json()['usageBreakdownList'][0]['currentUsageWithPrecision']
            check(model+' credits debited',after>before)
            req(public,'/',payload,headers)
            repeat=req(public,'/getUsageLimits',headers=auth).json()['usageBreakdownList'][0]['currentUsageWithPrecision']
            check(model+' replay not double charged',repeat==after)
        req(admin,'/api/v1/admin/cards/status',{'cardId':card['cardId'],'action':'ban','reason':'release smoke complete'});card=None
        req(public,'/List-Available-Models',headers=auth,expected=401)
        oldcookies=admin.cookies.copy()
        req(admin,'/api/v1/admin/session/revoke',{})
        admin.cookies=oldcookies
        req(admin,'/api/v1/admin/stats',expected=401)
        check('Logout invalidates copied session cookie',True)
    finally:
        try:
            if card:
                req(admin,'/api/v1/admin/cards/status',{'cardId':card['cardId'],'action':'ban','reason':'release smoke cleanup'})
                card=None
        finally:
            ssh.close();admin.close();public.close()
            (ROOT/'.acceptance/native-release-live.json').write_text(json.dumps({'checks':checks,'disposable_card_cleanup':card is None},indent=2),encoding='utf8')

if __name__=='__main__':main()
