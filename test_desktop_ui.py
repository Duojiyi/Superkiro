"""Browser regression using the actual local bridge and mocked CLI responses.
Does not activate cards or modify Kiro. Run: python test_desktop_ui.py
"""
import json, threading, unittest
from pathlib import Path
from unittest.mock import patch
from http.server import ThreadingHTTPServer
from playwright.sync_api import sync_playwright
import run_desktop as bridge

class DesktopUiTest(unittest.TestCase):
    def test_real_dom_workflows(self):
        server=ThreadingHTTPServer(('127.0.0.1',0),bridge.SecureBridgeHandler)
        worker=threading.Thread(target=server.serve_forever,daemon=True);worker.start()
        calls=[]
        activation_ok=False
        process_running=True
        usage_failed=False
        used_points=50.493568
        def cli(args,payload=None):
            calls.append((args,payload))
            if args[0]=='status':return 0,json.dumps({'kiro_installed':True,'kiro_version':'test','process_state':'Running' if process_running else 'Stopped','authenticated':activation_ok,'has_snapshot':activation_ok})
            if args[0]=='desktop-usage':return (1,json.dumps({'success':False,'error':'offline'})) if usage_failed else (0,json.dumps({'success':True,'usage':{'usageBreakdownList':[{'dimensionType':'CREDIT','usageLimitWithPrecision':200,'currentUsageWithPrecision':used_points}]}}))
            if args[0]=='doctor':return 0,json.dumps({'overall_status':'offline','items':[{'name':'Gateway','level':'fail','detail':'<script>unsafe</script> unreachable'}]})
            if args[0]=='desktop-activate' and activation_ok:return 0,json.dumps({'success':True})
            if args[0]=='desktop-activate':return 1,json.dumps({'success':False,'error':'Kiro is running. Please close Kiro before activating.'})
            return 0,json.dumps({'success':True,'total_memory_mb':125})
        try:
            with patch.object(bridge,'verify_card',return_value={'status':'active','virtualPlanName':'测试套餐','remainingPoints':150,'totalPoints':200,'validUntil':2000000000,'boundDevices':[],'maxDevices':2}), patch.object(bridge,'run_patch_cli',side_effect=cli),sync_playwright() as pw:
                browser=pw.chromium.launch()
                page=browser.new_page(viewport={'width':441,'height':541});errors=[]
                page.on('pageerror',lambda e:errors.append(str(e)))
                url=f'http://127.0.0.1:{server.server_port}/'
                page.goto(url+'#token='+bridge.SESSION_TOKEN)
                page.wait_for_function("() => document.getElementById('bridge-state').textContent === '本地服务已连接'")
                self.assertNotIn('token=',page.url)
                self.assertEqual(page.locator('#gateway-input').input_value(), bridge.DEFAULT_GATEWAY_URL)
                page.evaluate("document.getElementById('gateway-input').value = ''")
                page.get_by_label('卡密',exact=True).fill('test-default-card')
                page.get_by_role('button',name='验证卡密',exact=True).click()
                page.locator('#authorization-summary').wait_for(state='visible')
                self.assertFalse(any(a[0]=='desktop-activate' for a,p in calls))
                page.locator('#dismiss-message').click()
                page.get_by_role('button',name='接管 Kiro',exact=True).click()
                self.assertFalse(page.locator('#connection-dialog').is_visible())
                page.get_by_role('button',name='确认操作',exact=True).click()
                page.get_by_text('Kiro 正在运行，当前操作尚未执行。请先保存工作并退出 Kiro，再重试。',exact=True).wait_for()
                self.assertTrue(any(a==['desktop-activate','--gateway-url',bridge.DEFAULT_GATEWAY_URL] and p.get('close_kiro_confirmed') is True for a,p in calls))
                calls.clear()
                page.locator('#dismiss-message').click()

                self.assertEqual(page.locator('nav,footer').count(),0)
                self.assertEqual(page.locator('.app-header').count(),1)
                self.assertNotIn('Debug',page.title())
                for width,height in [(320,480),(441,541),(680,820),(1280,900)]:
                    page.set_viewport_size({'width':width,'height':height})
                    for name in ['connect','status','doctor','settings']:
                        page.evaluate('(name) => view(name)',name)
                        self.assertTrue(page.evaluate('document.documentElement.scrollWidth <= innerWidth'),(width,name))
                page.set_viewport_size({'width':441,'height':541})
                page.evaluate("view('status')")
                Path('.acceptance').mkdir(exist_ok=True)
                page.locator('#standby-art').screenshot(path='.acceptance/desktop-pencil-standby.png')
                page.evaluate("view('connect')")
                page.get_by_role('button',name='偏好设置',exact=True).click()
                page.get_by_role('button',name='网关地址与连接配置',exact=False).click()
                page.get_by_label('网关地址',exact=True).fill('https://gateway.invalid')
                page.get_by_role('button',name='完成',exact=True).click()
                page.get_by_role('button',name='返回',exact=True).click()
                page.get_by_label('卡密',exact=True).fill('test-only-card')
                page.get_by_role('button',name='显示',exact=True).click()
                self.assertEqual(page.locator('#card-input-field').get_attribute('type'),'text')
                page.get_by_role('button',name='隐藏',exact=True).click()
                page.get_by_role('button',name='验证卡密',exact=True).click()
                page.locator('#authorization-summary').wait_for(state='visible')
                page.locator('#dismiss-message').click()
                page.get_by_role('button',name='接管 Kiro',exact=True).click()
                page.get_by_role('button',name='取消',exact=True).click()
                self.assertFalse(any(a[0]=='desktop-activate' for a,p in calls))
                self.assertIn('150.000000 / 200.000000',page.locator('#authorization-summary').inner_text())
                self.assertIn('测试套餐',page.locator('#authorization-summary').inner_text())
                page.get_by_role('button',name='接管 Kiro',exact=True).click()
                page.get_by_role('button',name='确认操作',exact=True).click()
                page.get_by_text('Kiro 正在运行，当前操作尚未执行。请先保存工作并退出 Kiro，再重试。',exact=True).wait_for()
                self.assertTrue(page.locator('#status').is_visible())
                page.locator('#dismiss-message').click()
                page.get_by_role('button',name='偏好设置',exact=True).click()
                page.get_by_role('button',name='连接诊断',exact=False).click()
                page.get_by_role('button',name='开始诊断 · 查看检查结果',exact=True).click()
                page.get_by_text('<script>unsafe</script> unreachable',exact=True).wait_for()
                self.assertEqual(page.locator('#doctor-items script').count(),0)
                self.assertTrue(any(a==['doctor','--gateway-url','https://gateway.invalid'] for a,p in calls))
                page.locator('#doctor-dialog [data-close]').click()
                page.locator('#dismiss-message').click()
                page.get_by_role('button',name='返回上一页',exact=True).click()
                self.assertTrue(page.locator('#settings').is_visible())
                process_running=False
                page.get_by_role('button',name='刷新状态 / 查看详情',exact=True).click()
                page.locator('#launch-kiro').click()
                page.get_by_text('已发送 Kiro 启动请求。请在 IDE 中确认模型与对话可用。',exact=True).wait_for()
                self.assertTrue(any(a==['desktop-launch'] and p is None for a,p in calls))
                page.locator('#restore').click()
                page.keyboard.press('Escape')
                self.assertFalse(any(a[0]=='desktop-logout' for a,p in calls))
                page.locator('#status-dialog [data-close]').click()
                page.locator('#unbind').click()
                page.locator('#confirm-card').fill('test-unbind-card')
                page.get_by_role('button',name='确认操作',exact=True).click()
                page.get_by_text('操作已完成。重新接管时需要再次输入卡密。',exact=True).wait_for()
                self.assertTrue(any(a==['desktop-unbind'] and p=={'card_key':'test-unbind-card'} for a,p in calls))
                activation_ok=True
                page.locator('#dismiss-message').click()
                page.get_by_label('卡密',exact=True).fill('test-success-card')
                page.get_by_role('button',name='验证卡密',exact=True).click()
                page.locator('#authorization-summary').wait_for(state='visible')
                page.locator('#dismiss-message').click()
                page.get_by_role('button',name='接管 Kiro',exact=True).click()
                page.get_by_role('button',name='确认操作',exact=True).click()
                page.get_by_text('接管完成，已发送 Kiro 启动请求。请在 IDE 中验证模型列表和真实对话。',exact=True).wait_for()
                self.assertTrue(page.locator('#status').is_visible())
                self.assertEqual(page.locator('#card-input-field').input_value(),'')
                self.assertIn('测试套餐',page.locator('#active-authorization').inner_text())
                page.wait_for_function("() => document.getElementById('active-authorization').textContent.includes('149.506432')")
                self.assertIn('云端余额快照',page.locator('#active-authorization').inner_text())
                page.reload();page.wait_for_function("() => document.getElementById('bridge-state').textContent === '本地服务已连接'")
                page.locator('#status').wait_for(state='visible')
                self.assertEqual(page.locator('#card-input-field').input_value(),'')
                self.assertIn('测试套餐',page.locator('#active-authorization').inner_text())
                page.wait_for_function("() => document.getElementById('active-authorization').textContent.includes('149.506432')")
                self.assertIn('云端余额快照',page.locator('#active-authorization').inner_text())
                used_points=50.679474
                page.evaluate("refreshBalance()")
                self.assertIn('149.320526',page.locator('#active-authorization').inner_text())
                usage_failed=True
                page.evaluate("refreshBalance()")
                self.assertIn('最近刷新失败',page.locator('#active-authorization').inner_text())
                self.assertIn('149.320526',page.locator('#active-authorization').inner_text())
                self.assertFalse(errors,errors)
                out=Path('.acceptance');out.mkdir(exist_ok=True)
                for name in ['connect','status','doctor','settings']:
                    page.set_viewport_size({'width':720,'height':680})
                    page.evaluate('(name) => view(name)',name)
                    page.locator('#'+name).screenshot(path=str(out/f'desktop-pencil-{name}.png'))
                page.set_viewport_size({'width':441,'height':610})
                page.evaluate("view('connect')")
                page.screenshot(path=str(out/'desktop-debug-441.png'),full_page=True)
                for name,size in [('status',{'width':427,'height':546}),('doctor',{'width':691,'height':420}),('settings',{'width':690,'height':460})]:
                    page.set_viewport_size(size)
                    page.evaluate('(name) => view(name)',name)
                    page.screenshot(path=str(out/f'desktop-fixed-{name}.png'),full_page=True)
                browser.close()
        finally:
            server.shutdown();server.server_close();worker.join(5)

if __name__=='__main__':unittest.main()
