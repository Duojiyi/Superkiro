"""Revision-checked pricing publication. stdin SSH credentials; no card mutations."""
import json,sys,time,email.utils
from decimal import Decimal
from pathlib import Path
import requests
sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
from deploy.release_candidate import ROOT,pinned_connection,deployment_lock,write_remote
POLICY=json.loads((ROOT/'deploy/pricing_policy.json').read_text(encoding='utf8'))
FIELDS=('input','output','cache_creation','cache_read')
def version(model,rate_card,stamp):
    prices=POLICY['official_prices_per_m'][model]
    result={'id':f'official024-{model}-{stamp}','rate_card_id':rate_card,'model':model,'currency':'CNY','pricing_mode':'fixed','margin_multiplier':1.0,'per_call_credit':0,'effective_from_secs':stamp}
    for kind,price in zip(FIELDS,prices):
        result[kind+'_price_per_m']=float(Decimal(str(price))*Decimal('0.08'))
        result['fixed_'+kind+'_credit_per_m']=int(Decimal(str(price))*8*1_000_000)
    return result

def main():
    credentials=json.load(sys.stdin);ssh=pinned_connection(credentials)
    try:
      with deployment_lock(ssh):
        with ssh.open_sftp() as f: access=json.loads(f.open('/etc/kiro-byok/admin-access.json').read())
        with requests.Session() as h:
          h.trust_env=False;base='https://kiro.rent/api/v1/admin/'
          r=h.post(base+'session',json={'username':access['username'],'password':access['password']},headers={'Origin':'https://kiro.rent'},timeout=30)
          if r.status_code!=200:raise RuntimeError('Admin login failed')
          session=h.get(base+'session',timeout=30);session.raise_for_status()
          csrf=session.json()['csrfToken'];headers={'Origin':'https://kiro.rent','X-CSRF-Token':csrf}
          r=h.get(base+'commercial-config',timeout=30);r.raise_for_status();before=r.json()['config']
          now=int(email.utils.parsedate_to_datetime(r.headers['Date']).timestamp());stamp=now+30
          groups={g['id']:g for g in before['groups']}
          active=[m for m in before['models'] if m['visible']]
          if not active:raise RuntimeError('No active models')
          expected=[]
          for m in active:
            g=groups[m['group_id']]
            if m['target_model']!=m['exposed_model_id'] or m['fallback_chain'] or m['aliases'] or m['credit_multiplier']!=1 or g['margin_multiplier']!=1:
              raise RuntimeError('Unexpected mapping/multiplier; review required')
            if m['context_window']>200000:raise RuntimeError('Long context pricing needs separate support')
            expected.append(version(m['exposed_model_id'],g['rate_card_id'],stamp))
          # Re-running after a completed deployment must not silently add another version.
          if any(v['id'].startswith('official024-') for v in before['versions']):raise RuntimeError('Pricing already published; inspect live versions before any update')
          update={'expected_revision':before['revision'],'reason':'Approved: official USD numeric usage x8 credits; CNY procurement x0.08; tiers 1000/30,2000/55,5000/130,10000/250. Fixed rates avoid legacy FX. Standard cache only.','versions':expected}
          out=ROOT/'.acceptance';out.mkdir(exist_ok=True)
          (out/'pricing-before.json').write_text(json.dumps(before,ensure_ascii=False,indent=2),encoding='utf8')
          (out/'pricing-publication.json').write_text(json.dumps(update,indent=2),encoding='utf8')
          backup=f'/opt/kiro-byok/pricing-{stamp}'
          # Persist rollback evidence before sending a single non-retried mutation.
          write_remote(ssh,backup+'-before.json',json.dumps(before).encode())
          write_remote(ssh,backup+'-policy.json',json.dumps(POLICY).encode())
          write_remote(ssh,backup+'-publication.json',json.dumps(update).encode())
          response=h.post(base+'commercial-config',json=update,headers=headers,timeout=45)
          if response.status_code!=200:raise RuntimeError(f'Publication returned {response.status_code}; inspect before retrying')
          after=h.get(base+'commercial-config',timeout=30);after.raise_for_status();current=after.json()['config']
          live={v['id']:v for v in current['versions']}
          if any(live.get(v['id'])!=v for v in expected):raise RuntimeError('Readback differs from intended prices')
          wait=max(0,stamp-int(email.utils.parsedate_to_datetime(after.headers['Date']).timestamp())+1)
          time.sleep(wait)
          check=h.get(base+'commercial-config',timeout=30);check.raise_for_status()
          server_now=int(email.utils.parsedate_to_datetime(check.headers['Date']).timestamp())
          if server_now<stamp:raise RuntimeError('Activation not yet reached')
          current=check.json()['config']
          for v in expected:
            eligible=[x for x in current['versions'] if x['rate_card_id']==v['rate_card_id'] and x['model']==v['model'] and x['effective_from_secs']<=server_now]
            if max(eligible,key=lambda x:x['effective_from_secs'])!=v:raise RuntimeError('Unexpected effective price')
          report={'status':'active','effective_from_secs':stamp,'revision':current['revision'],'model_count':len(expected),'policy':POLICY,'checks':['revision-checked publication','all published fields match','effective versions match for all visible models'],'limitations':['standard cache creation only; no 1h TTL breakdown','existing financial face-value setting is not sales revenue','tier prices are catalog prices, not payment receipts']}
          print(json.dumps(report,ensure_ascii=False),flush=True)
          (out/'pricing-live.json').write_text(json.dumps(report,ensure_ascii=False,indent=2),encoding='utf8')
          h.post(base+'session/revoke',json={},headers=headers,timeout=15)
    finally:ssh.close()
if __name__=='__main__':main()
