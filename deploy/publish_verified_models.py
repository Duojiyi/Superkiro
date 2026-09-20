"""Publish verified testing models without changing commercial rates or card balances.
Pinned SSH password is read from stdin; upstream and administrator secrets stay in memory.
"""
import json, sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import requests
from test_deployed_server import connect, ROOT, HOST

def main():
    ssh=connect(json.load(sys.stdin)['password'],False)
    try:
        with ssh.open_sftp() as f:
            env=dict(l.split('=',1) for l in f.open('/etc/kiro-byok/gateway.env').read().decode().splitlines() if '=' in l)
        models=['claude-sonnet-4-6','claude-opus-4-6','claude-opus-4-7','claude-opus-4-8','claude-opus-5','claude-sonnet-5']
        text=json.loads((ROOT/'.acceptance/upstream-model-probes.json').read_text())
        tools=json.loads((ROOT/'.acceptance/upstream-tool-probes.json').read_text())
        assert all(any(x['model']==m and x.get('status')==200 and x.get('has_text') for x in text) and any(x['model']==m and x.get('tool_use') for x in tools) for m in models)
        with requests.Session() as s:
            s.trust_env=False;s.verify=str(ROOT/'deploy/server-ca.pem');base='https://'+HOST
            def req(path,data=None,headers=None):
                r=s.request('GET' if data is None else 'POST',base+'/api/v1/admin/'+path,json=data,headers=headers,timeout=30)
                assert r.status_code==200, f'{path}: HTTP {r.status_code}'
                return r.json()
            auth={'Authorization':'Bearer '+req('session',{}, {'x-admin-key':env['ADMIN_KEY']})['accessToken']}
            before=req('commercial-config',headers=auth)['config']
            (ROOT/'.acceptance/models-config-before.json').write_text(json.dumps(before,indent=2),encoding='utf8')
            req('providers/import',{'format':'cc_switch','content':{'providers':[{'id':'kimera-primary','name':'Kimera verified testing models','api_type':'anthropic','base_url':env['UPSTREAM_BASE_URL'],'api_key':env['UPSTREAM_API_KEY'],'models':models}]},'target_group_id':'group-pro-plus'},auth)
            config=req('commercial-config',headers=auth)['config']
            mappings=[{'id':'kimera-test-'+m,'group_id':'group-pro-plus','exposed_model_id':m,'target_provider_id':'kimera-primary','target_model':m,'context_window':200000,'max_output':8192,'supports_tools':True,'supports_vision':False,'supports_reasoning':False,'credit_multiplier':1.0,'visible':True,'sort_order':i,'aliases':[],'fallback_chain':[]} for i,m in enumerate(models)]
            published=req('commercial-config',{'expected_revision':config['revision'],'reason':'Enable six text/tool-probed models for acceptance. Preserve existing TEST rates; commercial prices and unverified capabilities are not published.','models':mappings},auth)['config']
            result={'revision':published['revision'],'models':models,'rates_changed':False,'pricing_status':'existing testing rates; not approved for commercial sales'}
            (ROOT/'.acceptance/models-published.json').write_text(json.dumps(result,indent=2),encoding='utf8')
            print(json.dumps(result))
    finally: ssh.close()
if __name__=='__main__':main()
