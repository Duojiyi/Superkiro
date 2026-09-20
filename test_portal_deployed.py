"""Real production browser checks using pinned TLS and a disposable card."""
import json,sys,uuid
from pathlib import Path
import requests
from playwright.sync_api import sync_playwright
from test_deployed_server import connect,HOST,PROXY,ROOT
cfg=json.load(sys.stdin);ssh=connect(cfg['password']);sftp=ssh.open_sftp();env=dict(l.split('=',1) for l in sftp.open('/etc/kiro-byok/gateway.env').read().decode().splitlines() if '=' in l and not l.startswith('#'))
_,out,_=ssh.exec_command('openssl s_client -connect 127.0.0.1:443 -servername '+HOST+' -CAfile /etc/kiro-byok/root.crt -verify_return_error </dev/null 2>/dev/null | openssl x509 -pubkey -noout | openssl pkey -pubin -outform DER | openssl dgst -sha256 -binary | openssl base64 -A');spki=out.read().decode().strip();assert len(spki)==44
sftp.close();ssh.close()
s=requests.Session();s.trust_env=False;s.proxies={'http':PROXY,'https':PROXY};s.verify=str(ROOT/'deploy/server-ca.pem')
def post(path,data,headers=None):
 r=s.post('https://'+HOST+path,json=data,headers=headers,timeout=40);assert r.status_code==200,(path,r.status_code);return r.json()
admin={'Authorization':'Bearer '+post('/api/v1/admin/session',{}, {'x-admin-key':env['ADMIN_KEY']})['accessToken']}
card=post('/api/v1/cards/pull',{'order_id':'browser-portal-v2-'+uuid.uuid4().hex,'count':1},{'X-Card-Platform-Key':env['CARD_PLATFORM_KEY']})['cards'][0]
result={};errors=[]
try:
 with sync_playwright() as p:
  b=p.chromium.launch(proxy={'server':PROXY},args=['--ignore-certificate-errors-spki-list='+spki])
  page=b.new_page(viewport={'width':1280,'height':960});page.on('pageerror',lambda e:errors.append(str(e)))
  page.goto('https://'+HOST+'/portal',wait_until='networkidle');assert page.locator('h1').inner_text()=='专注创造，服务在这里。'
  page.screenshot(path='.acceptance/portal-v2-live-desktop.png',full_page=True)
  page.locator('#query-card').fill(card['raw_code']);page.locator('#panel-query [type=submit]').click();page.get_by_text('授权信息已更新',exact=True).wait_for();assert '未激活' in page.locator('#result-query').inner_text()
  page.locator('#tab-activate').click();page.locator('#activate-card').fill(card['raw_code']);page.locator('#activate-device').fill('portal-browser-test');page.locator('#panel-activate [type=submit]').click();page.locator('#confirm-ok').click();page.get_by_text('云端授权已激活',exact=True).wait_for()
  page.locator('#tab-query').click();page.locator('#panel-query [type=submit]').click();page.get_by_role('button',name='管理此设备').wait_for();page.get_by_role('button',name='管理此设备').click()
  page.locator('#panel-unbind [type=submit]').click();page.locator('#confirm-cancel').click();assert 'portal-browser-test' in post('/api/v1/portal/query',{'card':card['raw_code']})['boundDevices']
  page.locator('#panel-unbind [type=submit]').click();page.locator('#confirm-ok').click();page.get_by_text('旧设备已解绑',exact=True).wait_for();assert not post('/api/v1/portal/query',{'card':card['raw_code']})['boundDevices']
  page.locator('#tab-topup').click();page.locator('#topup-card').fill(card['raw_code']);page.locator('#topup-code').fill('invalid-browser-probe-'+uuid.uuid4().hex);page.locator('#panel-topup [type=submit]').click();page.locator('#confirm-ok').click();page.get_by_text('操作未完成',exact=True).wait_for();assert page.locator('#topup-code').input_value()
  page.reload();page.set_viewport_size({'width':375,'height':812})
  for action in ['query','activate','unbind','topup']:
   page.locator('#tab-'+action).click();assert page.evaluate('document.documentElement.scrollWidth<=innerWidth')
  page.locator('#tab-query').click();page.screenshot(path='.acceptance/portal-v2-live-mobile.png',full_page=True)
  page.get_by_role('button',name='使用指南 ↗').last.click();assert page.locator('#help-dialog').is_visible();page.locator('#help-close').click()
  assert not errors,errors
  result={'query':True,'activate':True,'cancel_preserves_binding':True,'unbind':True,'invalid_topup_rejected':True,'mobile_four_tabs_no_overflow':True,'help':True,'page_errors':errors,'tls':'SSH-pinned server SPKI','real_topup_success':'covered by Rust integration and mocked browser test; not a live redemption'}
  b.close()
finally:
 post('/api/v1/admin/cards/status',{'cardId':card['card_id'],'action':'ban','reason':'production browser acceptance cleanup'},admin)
 s.close();Path('deployment-portal-v2-browser-results.json').write_text(json.dumps(result,ensure_ascii=False,indent=2),encoding='utf-8')
print(json.dumps(result,ensure_ascii=False),flush=True)
