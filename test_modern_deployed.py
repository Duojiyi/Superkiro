"""Probe Kiro 1.1 wire routes against production using one disposable card.
Input: stdin JSON with SSH password. Never persists credentials or customer data.
"""
import json, sys, uuid
from test_deployed_server import connect, ROOT, HOST
from test_single_server_smoke import decode_frames
import requests


def main():
    ssh = connect(json.load(sys.stdin)['password'], use_proxy=False)
    session = requests.Session()
    session.trust_env = False
    session.verify = str(ROOT / 'deploy/server-ca.pem')
    card = None
    results = []
    def req(path, data=None, headers=None, expected=200):
        r = session.request('GET' if data is None else 'POST', 'https://' + HOST + path,
                            json=data, headers=headers, timeout=(15, 120), allow_redirects=False)
        assert r.status_code == expected, f'{path}: expected {expected}, got {r.status_code}'
        return r
    def check(name, ok):
        assert ok, name
        results.append({'check': name, 'passed': True})
        print('PASS: ' + name, flush=True)
    try:
        with ssh.open_sftp() as sftp:
            env = dict(line.split('=', 1) for line in sftp.open('/etc/kiro-byok/gateway.env').read().decode().splitlines() if '=' in line)
        admin = {'Authorization': 'Bearer ' + req('/api/v1/admin/session', {}, {'x-admin-key':env['ADMIN_KEY']}).json()['accessToken']}
        req('/List-Available-Models', expected=401)
        req('/', {}, {'x-amz-target':'KiroRuntimeService.GenerateAssistantResponse'}, expected=401)
        check('Modern routes reject anonymous access', True)
        card = req('/api/v1/cards/pull', {'order_id':'modern-probe-'+uuid.uuid4().hex, 'count':1}, {'X-Card-Platform-Key':env['CARD_PLATFORM_KEY']}).json()['cards'][0]
        auth = {'Authorization':'Bearer '+req('/oauth/token', {'card_key':card['raw_code'], 'device_id':'modern-probe-'+uuid.uuid4().hex}).json()['accessToken']}
        def rpc(op, data, invocation=None):
            headers = {**auth, 'x-amz-target':op, 'content-type':'application/x-amz-json-1.0'}
            if invocation:
                headers['amz-sdk-invocation-id'] = invocation
            return req('/', data, headers)
        legacy = req('/ListAvailableModels', headers=auth).json()
        models = req('/List-Available-Models?origin=AI_EDITOR', headers=auth).json()
        check('Modern CPS models return object defaultModel and preserve legacy models', models['models']==legacy['models'] and models['defaultModel']['modelId']==legacy['defaultModel'])
        check('CPS JSON RPC model operation', rpc('KiroControlPlaneBearerService.ListAvailableModels', {}).json()==models)
        check('Runtime feature configuration', rpc('KiroRuntimeService.GetFeatureConfiguration', {'feature':'test','version':'1'}).json()=={'configuration':{}})
        tools = rpc('KiroRuntimeService.InvokeMCP', {'jsonrpc':'2.0','id':'probe','method':'tools/list','params':{}}).json()
        check('Runtime tool discovery', isinstance(tools.get('result',{}).get('tools'),list))
        before = req('/api/v1/portal/query', {'card':card['raw_code']}).json()['remainingCredits']
        payload = {'conversationState':{'conversationId':'modern-'+uuid.uuid4().hex,'history':[], 'currentMessage':{'userInputMessage':{'content':'Reply only with OK.', 'modelId':models['defaultModel']['modelId']}}}}
        payload['systemPrompt'] = 'You are a concise assistant. Follow the requested reply format.'
        invalid = {**payload, 'additionalModelRequestFields': {'output_config': {'effort': 'invalid-tier'}}}
        req('/', invalid, {**auth, 'x-amz-target':'KiroRuntimeService.GenerateAssistantResponse'}, expected=400)
        rejected_balance = req('/api/v1/portal/query', {'card':card['raw_code']}).json()['remainingCredits']
        check('Invalid reasoning tier is rejected without debit', rejected_balance == before)
        invocation = uuid.uuid4().hex
        r = rpc('KiroRuntimeService.GenerateAssistantResponse', payload, invocation)
        frames = decode_frames(r.content)
        check('Runtime JSON RPC reaches real upstream and returns AWS EventStream', 'application/vnd.amazon.eventstream' in r.headers.get('Content-Type','') and 'OK' in ''.join(f.get('content', '') for f in frames) and any(f.get('stopReason') == 'end_turn' for f in frames))
        after = req('/api/v1/portal/query', {'card':card['raw_code']}).json()['remainingCredits']
        check('Runtime request debits disposable card', after < before)
        rpc('KiroRuntimeService.GenerateAssistantResponse', payload, invocation)
        balance = req('/api/v1/portal/query', {'card':card['raw_code']}).json()['remainingCredits']
        check('Runtime duplicate invocation does not debit twice', balance == after)
        req('/', {}, {**auth,'x-amz-target':'OtherService.GenerateAssistantResponse'}, expected=404)
        check('Unknown service cannot invoke runtime', True)
    finally:
        try:
            if card:
                req('/api/v1/admin/cards/status', {'cardId':card['card_id'],'action':'ban','reason':'modern protocol acceptance completed'}, admin)
                req('/List-Available-Models', headers=auth, expected=401)
                check('Disposable card banned and modern route token revoked', True)
        finally:
            (ROOT/'.acceptance/modern-deployed-results.json').write_text(json.dumps(results,indent=2),encoding='utf8')
            ssh.close()
            session.close()

if __name__ == '__main__': main()
