"""Portal UI contract tests with mocked API; never consumes real cards."""
import json,threading,unittest
from pathlib import Path
from http.server import SimpleHTTPRequestHandler,ThreadingHTTPServer
from functools import partial
from playwright.sync_api import sync_playwright

class PortalUiTest(unittest.TestCase):
 def test_workflows(self):
  class Quiet(SimpleHTTPRequestHandler):
   def log_message(self,*args):pass
  server=ThreadingHTTPServer(('127.0.0.1',0),partial(Quiet,directory=str(Path('apps/portal-ui').resolve())))
  worker=threading.Thread(target=server.serve_forever,daemon=True);worker.start()
  calls=[];fail=False
  def respond(route):
   nonlocal fail
   action=route.request.url.rsplit('/',1)[-1];data=route.request.post_data_json;calls.append((action,data))
   if fail:route.fulfill(status=429,headers={'Retry-After':'30'},json={'success':False});return
   result={'success':True}
   if action=='query':result.update(status='active',remainingPoints=20,validUntil=1800000000,maxDevices=2,rebindCount=0,maxRebinds=5,boundDevices=['device-test'],virtualPlanName='<img src=x onerror=alert(1)>')
   if action=='challenge':result['challengeToken']='test-challenge'
   if action=='activate':result.update(validUntil=1800000000,remainingPoints=20)
   if action=='unbind':result['remainingDevices']=[]
   if action=='topup':result.update(addedPoints=10,remainingPoints=30)
   route.fulfill(json=result)
  try:
   with sync_playwright() as p:
    browser=p.chromium.launch();page=browser.new_page(viewport={'width':1280,'height':960});errors=[]
    page.on('pageerror',lambda e:errors.append(str(e)))
    page.route('**/api/v1/portal/*',respond)
    page.goto(f'http://127.0.0.1:{server.server_port}/index.html')
    self.assertEqual(page.locator('#query-card').get_attribute('type'),'password')
    page.locator('#query-card').fill('card-test');page.locator('#panel-query [type=submit]').click()
    page.get_by_text('授权信息已更新',exact=True).wait_for()
    self.assertEqual(page.locator('#result-query img').count(),0)
    self.assertIn('<img src=x onerror=alert(1)>',page.locator('#result-query').inner_text())
    page.get_by_role('button',name='管理此设备').click()
    self.assertEqual(page.locator('#unbind-device').input_value(),'device-test')
    page.locator('#panel-unbind [type=submit]').click();page.locator('#confirm-cancel').click()
    self.assertFalse(any(a=='unbind' for a,_ in calls))
    page.locator('#panel-unbind [type=submit]').click();page.locator('#confirm-ok').click()
    page.get_by_text('旧设备已解绑',exact=True).wait_for()
    self.assertIn(('challenge',{'action':'unbind'}),calls)
    self.assertEqual([d for a,d in calls if a=='unbind'][0]['challenge_token'],'test-challenge')
    page.locator('#tab-activate').click();page.locator('#activate-card').fill('card-test')
    page.locator('#panel-activate [type=submit]').click();page.locator('#confirm-ok').click();page.get_by_text('云端授权已激活',exact=True).wait_for()
    page.locator('#tab-topup').click();page.locator('#topup-card').fill('card-test');page.locator('#topup-code').fill('voucher-test')
    page.locator('#panel-topup [type=submit]').click();page.locator('#confirm-ok').click();page.get_by_text('充值已到账',exact=True).wait_for()
    self.assertEqual(page.locator('#topup-code').input_value(),'')
    fail=True;page.locator('#tab-query').click();page.locator('#panel-query [type=submit]').click()
    page.get_by_text('操作未完成',exact=True).wait_for();self.assertIn('30 秒',page.locator('#result-query').inner_text());self.assertEqual(page.locator('#query-card').input_value(),'card-test')
    self.assertTrue(page.evaluate('localStorage.length===0 && sessionStorage.length===0'))
    page.reload();page.screenshot(path='.acceptance/portal-v2-local-desktop.png',full_page=True)
    for width in [320,375,768,1280]:
     page.set_viewport_size({'width':width,'height':900})
     for action in ['query','activate','unbind','topup']:
      page.locator('#tab-'+action).click();self.assertTrue(page.evaluate('document.documentElement.scrollWidth<=innerWidth'),(width,action))
    page.set_viewport_size({'width':375,'height':812});page.locator('#tab-query').click();page.screenshot(path='.acceptance/portal-v2-local-mobile.png',full_page=True)
    self.assertFalse(errors,errors);browser.close()
  finally:server.shutdown();server.server_close();worker.join(5)

if __name__=='__main__':unittest.main()
