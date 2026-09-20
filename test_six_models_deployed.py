"""Six-model production protocol acceptance with a disposable, finally banned card."""
import json,sys,uuid
from pathlib import Path
import requests
from test_deployed_server import connect,ROOT,HOST
from test_single_server_smoke import decode_frames

def main():
    ssh=connect(json.load(sys.stdin)['password'],False)
    s=requests.Session();s.trust_env=False;s.verify=str(ROOT/'deploy/server-ca.pem');card=None;checks=[]
    try:
        with ssh.open_sftp() as f:env=dict(l.split('=',1) for l in f.open('/etc/kiro-byok/gateway.env').read().decode().splitlines() if '=' in l)
        def req(path,data=None,headers=None):
            r=s.request('GET' if data is None else 'POST','https://'+HOST+path,json=data,headers=headers,timeout=(15,180));assert r.status_code==200,f'{path}: HTTP {r.status_code}';return r
        admin={'Authorization':'Bearer '+req('/api/v1/admin/session',{}, {'x-admin-key':env['ADMIN_KEY']}).json()['accessToken']}
        card=req('/api/v1/cards/pull',{'order_id':'six-model-probe-'+uuid.uuid4().hex,'count':1},{'X-Card-Platform-Key':env['CARD_PLATFORM_KEY']}).json()['cards'][0]
        auth={'Authorization':'Bearer '+req('/oauth/token',{'card_key':card['raw_code'],'device_id':'six-model-'+uuid.uuid4().hex}).json()['accessToken']}
        expected=['claude-sonnet-4-6','claude-opus-4-6','claude-opus-4-7','claude-opus-4-8','claude-opus-5','claude-sonnet-5']
        models=req('/List-Available-Models',headers=auth).json();assert [m['modelId'] for m in models['models']]==expected
        assert models['defaultModel']['modelId']=='claude-sonnet-4-6'
        print('PASS six models and stable default',flush=True)
        for model in expected:
            before=req('/getUsageLimits',headers=auth).json()['usageBreakdownList'][0]['currentUsageWithPrecision']
            invocation=uuid.uuid4().hex
            headers={**auth,'x-amz-target':'KiroRuntimeService.GenerateAssistantResponse','amz-sdk-invocation-id':invocation}
            payload={'conversationState':{'conversationId':'six-model-'+uuid.uuid4().hex,'history':[],'currentMessage':{'userInputMessage':{'content':'Reply only with OK.','modelId':model}}}}
            frames=decode_frames(req('/',payload,headers).content)
            assert 'ok' in ''.join(f.get('content','') for f in frames).lower(),(model,frames)
            assert any(f.get('stopReason')=='end_turn' for f in frames),model
            after=req('/getUsageLimits',headers=auth).json()['usageBreakdownList'][0]['currentUsageWithPrecision'];assert after>before,model
            req('/',payload,headers)
            replay=req('/getUsageLimits',headers=auth).json()['usageBreakdownList'][0]['currentUsageWithPrecision'];assert replay==after,model
            checks.append({'model':model,'reply_ok':True,'credits_debited':round(after-before,6),'duplicate_no_debit':True})
            print('PASS '+model+' debit='+str(round(after-before,6))+' replay idempotent',flush=True)
    finally:
        try:
            if card:
                req('/api/v1/admin/cards/status',{'cardId':card['card_id'],'action':'ban','reason':'six-model acceptance completed'},admin)
                r=s.get('https://'+HOST+'/List-Available-Models',headers=auth,timeout=20);assert r.status_code==401
                print('PASS disposable card banned and token revoked',flush=True)
        finally:
            (ROOT/'.acceptance/six-model-deployed-results.json').write_text(json.dumps(checks,indent=2),encoding='utf8');ssh.close();s.close()
if __name__=='__main__':main()
