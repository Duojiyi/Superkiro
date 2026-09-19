"""Superkiro frontend-only tests. All HTTP is intercepted; no runner or production calls.
Run: python -B -m unittest -v test_superkiro_desktop_ui
Screenshots: set SUPERKIRO_UI_SCREENSHOTS to a temporary output directory.
"""
import json
import os
from pathlib import Path
import tempfile
import unittest
from playwright.sync_api import sync_playwright, expect

ROOT = Path(__file__).resolve().parent
UI = ROOT / 'apps' / 'desktop-ui'


class SuperkiroDesktopUI(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.pw = sync_playwright().start()
        cls.browser = cls.pw.chromium.launch()
        cls.output = Path(os.environ.get('SUPERKIRO_UI_SCREENSHOTS', tempfile.gettempdir() + '/superkiro-ui-verification'))
        cls.output.mkdir(parents=True, exist_ok=True)

    @classmethod
    def tearDownClass(cls):
        cls.browser.close()
        cls.pw.stop()

    def setUp(self):
        self.context = self.browser.new_context(viewport={'width':620,'height':820}, reduced_motion='reduce', accept_downloads=True)
        self.page = self.context.new_page()
        self.errors, self.calls = [], []
        self.page.on('pageerror', lambda error: self.errors.append(str(error)))
        self.state = {'platform':'win32', 'kiro_installed':True, 'kiro_version':'test', 'kiro_install_path':'C:/Users/private/Kiro.exe', 'process_state':'Stopped', 'authenticated':False, 'has_snapshot':False}
        self.auth = {'virtualPlanName':'PRO', 'remainingPoints':842.5, 'totalPoints':1000, 'status':'active', 'validUntil':2000000000, 'isExpired':False}
        self.usage_extra = {}
        self.used, self.fail_usage, self.fail_restore, self.fail_login = 157.5, False, False, False
        self.page.add_init_script("""window.nativeCalls=[]; window.pywebview={api:{screen:(...a)=>window.nativeCalls.push(['screen',...a]),minimize:(...a)=>window.nativeCalls.push(['minimize',...a]),maximize:(...a)=>window.nativeCalls.push(['maximize',...a]),close:(...a)=>window.nativeCalls.push(['close',...a]),pick_install_path:(...a)=>{window.nativeCalls.push(['pick_install_path',...a]);return {success:true,path:'C:/Kiro/Kiro.exe'}}}};""")
        self.context.route('**/*', self.route)
        self.page.goto('http://superkiro.test/#token=local-test-token')
        self.page.wait_for_function("document.getElementById('install-state').textContent.includes('已自动识别')")

    def tearDown(self):
        self.assertEqual(self.errors, [])
        self.context.close()

    def route(self, route):
        request = route.request
        path = request.url.split('superkiro.test')[-1].split('?')[0]
        if path.startswith('/api/'):
            self.assertEqual(request.headers.get('x-kiro-session-token'), 'local-test-token')
            body = request.post_data_json if request.post_data else None
            self.calls.append((path, body))
            response, status = {}, 200
            if path == '/api/status': response = self.state
            elif path == '/api/heartbeat': response = {'status':'alive'}
            elif path == '/api/verify-card':
                response = {'success':True,'authorization':self.auth,'gateway_url':'https://gateway.invalid'}
                if self.fail_login: response, status = {'success':False,'error':'invalid card SECRET https://private.invalid'}, 400
            elif path == '/api/activate':
                self.state.update(authenticated=True,has_snapshot=True,process_state='Running')
                response = {'success':True}
            elif path in ['/api/restore','/api/unbind']:
                response = {'success':not self.fail_restore}
                if self.fail_restore: response['error'] = 'restore failed private secret'
                else: self.state.update(authenticated=False,has_snapshot=False)
            elif path == '/api/usage':
                response = {'success':True,'usage':{'usageBreakdownList':[{'dimensionType':'CREDIT','currentUsageWithPrecision':self.used,'usageLimitWithPrecision':1000}]}}
                response['usage'].update(self.usage_extra)
                if self.fail_usage: response, status = {'success':False,'error':'offline'}, 503
            elif path == '/api/doctor': response = {'items':[{'name':'Kiro Installation','level':'pass','detail':'C:/Users/private secret-card'},{'name':'Gateway Connectivity','level':'fail','detail':'https://10.0.0.1 upstream-secret'},{'name':'<script>secret</script>','level':'warning','detail':'Bearer SECRET'}]}
            elif path == '/api/memory/sample': response = {'total_memory_mb':1240,'agent_memory_mb':240}
            elif path in ['/api/launch','/api/memory/trim']: response = {'success':True}
            else: self.fail('Unapproved endpoint: '+path)
            route.fulfill(status=status,content_type='application/json',body=json.dumps(response))
        else:
            filename = 'index.html' if path == '/' else path.lstrip('/')
            self.assertIn(filename, ['index.html','desktop.css','desktop.js','favicon.ico'])
            if filename == 'favicon.ico': route.fulfill(status=204); return
            route.fulfill(content_type={'index.html':'text/html','desktop.css':'text/css','desktop.js':'application/javascript'}[filename],body=(UI / filename).read_text(encoding='utf-8-sig'))

    def login(self):
        self.page.locator('#card-input-field').fill('test-card-secret')
        self.page.locator('#login-submit').click()
        expect(self.page.locator('#status')).to_be_visible()
        self.page.wait_for_function('!busy')

    def confirm(self):
        expect(self.page.locator('#confirm-cancel')).to_be_focused()
        self.page.evaluate("window.confirmClosed=false; document.getElementById('confirm-dialog').addEventListener('close',()=>window.confirmClosed=true,{once:true})")
        self.page.locator('#confirm-submit').click()
        self.page.wait_for_function('window.confirmClosed && !busy && !document.getElementById("confirm-dialog").open')

    def shot(self, name):
        if self.page.locator('#dismiss-message').is_visible() and not self.page.locator('dialog[open]').count():
            self.page.locator('#dismiss-message').click()
        self.page.screenshot(path=str(self.output / (name+'.png')), full_page=True)

    def test_login_read_only_four_plans_and_secret_storage(self):
        self.page.set_viewport_size({'width':480,'height':620})
        self.shot('01-login')
        self.assertNotIn('token=', self.page.url)
        self.assertTrue(self.page.locator('#remember-card').is_disabled())
        self.login()
        self.assertFalse(any(path == '/api/activate' for path,_ in self.calls))
        self.assertEqual(self.page.locator('#card-input-field').input_value(), '')
        stored = self.page.evaluate('JSON.stringify({...localStorage,...sessionStorage})')
        self.assertNotIn('test-card-secret', stored)
        for plan in ['PRO','PRO+','PRO Max','Power']:
            self.page.evaluate('(name)=>{authorizationInfo.virtualPlanName=name;renderAuthorization()}',plan)
            self.assertTrue(self.page.locator('#plan-summary').inner_text().startswith(plan+' /'))
        self.page.evaluate("authorizationInfo.virtualPlanName='Enterprise';renderAuthorization()")
        self.assertIn('套餐待查询', self.page.locator('#plan-summary').inner_text())

    def test_activation_confirmation_no_false_ready_and_restore(self):
        self.login()
        self.shot('03-pending')
        self.page.locator('#takeover').click()
        expect(self.page.locator('#confirm-dialog')).to_contain_text('配置安全连接')
        expect(self.page.locator('#confirm-dialog')).not_to_contain_text('安装本机证书')
        self.shot('14-confirm')
        self.page.keyboard.press('Escape')
        self.assertFalse(any(path == '/api/activate' for path,_ in self.calls))
        self.page.locator('#takeover').click(); self.confirm()
        expect(self.page.locator('#overview-title')).to_have_text('连接配置已应用')
        payload = next(body for path,body in self.calls if path == '/api/activate')
        self.assertEqual(payload, {'gateway_url':'https://gateway.invalid','card_key':'test-card-secret','close_kiro_confirmed':True})
        self.state['model_service_available'] = True
        self.page.evaluate('status()')
        expect(self.page.locator('#overview-title')).to_have_text('Kiro 已就绪')
        self.shot('02-connected')
        # System fonts differ between Windows and Linux. Test the layout contract,
        # not an absolute coordinate accumulated from platform-specific line boxes.
        for font in [None, 'sans-serif', 'serif']:
            with self.subTest(font=font):
                self.page.evaluate("font => document.documentElement.style.fontFamily = font || ''", font)
                wave = self.page.locator('#status .overview-wave').bounding_box()
                actions = self.page.locator('#status .actions').bounding_box()
                expect(self.page.locator('#activation-note')).to_be_hidden()
                balance = self.page.locator('#status .balance').bounding_box()
                self.assertAlmostEqual(wave['height'], 121, delta=0.1)
                self.assertAlmostEqual(actions['y'], wave['y'] + wave['height'], delta=0.1)
                self.assertGreaterEqual(actions['height'], 50)
                self.assertAlmostEqual(balance['y'] - actions['y'] - actions['height'], 21, delta=0.1)
        self.page.evaluate("document.documentElement.style.fontFamily = ''")
        heights = self.page.locator('.overview-wave i').evaluate_all('(bars)=>bars.map(b=>b.getBoundingClientRect().height)')
        self.assertGreater(heights[29], heights[11])
        self.assertGreater(heights[11], heights[47])
        self.assertLessEqual(self.page.locator('.overview-footer').bounding_box()['y'] + self.page.locator('.overview-footer').bounding_box()['height'], 820)
        self.page.locator('#overview-secondary').click(); self.confirm()
        expect(self.page.locator('#connect')).to_be_visible()

    def test_usage_empty_failure_no_fabrication_and_real_chart(self):
        self.login(); self.page.locator('nav [data-view=usage]').click()
        expect(self.page.locator('#settled-points')).to_have_text('157.5')
        expect(self.page.locator('.usage-totals')).to_contain_text('累计已结算积分（生命周期）')
        expect(self.page.locator('.usage-totals')).to_contain_text('Tokens 总计（近30天）')
        expect(self.page.locator('#chart-title')).to_have_text('每日消耗（近7天 · UTC）')
        expect(self.page.locator('#usage .divider h2')).to_have_text('按模型（近30天 · UTC）')
        self.assertIn('今日消耗（UTC）', self.page.locator('.metrics').text_content())
        self.assertIn('今日 Tokens（UTC）', self.page.locator('.metrics').text_content())
        expect(self.page.locator('#price-note')).to_have_text('美元参考价未配置，暂不提供估算。')
        expect(self.page.locator('#settled-tokens')).to_have_text('—')
        self.assertIn('暂未提供', self.page.locator('#usage-chart').inner_text())
        self.page.locator('[data-unit=usd]').click()
        self.assertIn('未配置', self.page.locator('#usage-chart').inner_text())
        self.page.evaluate("usageData.settledUsage={daily:[{date:'2026-09-18',points:12.4,tokens:30},{date:'2026-09-19',points:24.8,tokens:50}],models:[{name:'Model A',points:37.2,tokens:80}]};usageUnit='points';renderUsageChart()")
        self.assertEqual(self.page.locator('.chart-column').count(),2)
        self.shot('04-usage')
        self.used = 0; self.page.locator('#refresh-usage').click()
        expect(self.page.locator('#usage-empty-title')).to_have_text('还没有用量记录'); self.shot('12-usage-empty')
        self.fail_usage = True; self.page.locator('#refresh-usage').click()
        expect(self.page.locator('#usage-empty-title')).to_have_text('用量加载失败')

    def test_real_usage_subscription_title_and_missing_settled_details(self):
        self.login()
        for plan in ['PRO', 'PRO+', 'PRO Max', 'Power']:
            self.usage_extra = {'subscriptionInfo': {'subscriptionTitle': plan}}
            self.page.evaluate('loadUsage()')
            expect(self.page.locator('#plan-summary')).to_contain_text(plan + ' /')
        self.usage_extra = {'subscriptionInfo': {'subscriptionTitle': 'Enterprise'}}
        self.page.evaluate('loadUsage()')
        expect(self.page.locator('#plan-summary')).to_contain_text('Power /')
        self.page.locator('nav [data-view=usage]').click()
        for unit in ['points', 'tokens', 'usd']:
            self.page.locator('[data-unit=' + unit + ']').click()
            self.assertEqual(self.page.locator('.chart-column').count(), 0)
        expect(self.page.locator('#settled-tokens')).to_have_text('—')
        expect(self.page.locator('#usage-models')).to_have_text('桥接暂未提供按模型明细。')
        self.page.evaluate("usageData.settledUsage={referencePrice:0.01,daily:[{date:'2026-09-19',points:2,tokens:10,usd:0.02}]};renderUsageChart()")
        expect(self.page.locator('#price-note')).to_contain_text('每积分 $0.01')
        self.assertEqual(self.page.locator('.chart-column').count(), 1)
        self.page.evaluate('loadUsage()')
        self.assertEqual(self.page.locator('.chart-column').count(), 0)
        expect(self.page.locator('#price-note')).to_contain_text('未配置')

    def test_window_close_safe_exit_and_usage_expiry_refresh(self):
        expect(self.page.locator('#window-close')).to_have_attribute('aria-label', '关闭')
        expect(self.page.locator('#window-close')).to_have_attribute('title', '关闭')
        self.page.locator('#window-close').click()
        self.assertIn(['close', 'local-test-token'], self.page.evaluate('nativeCalls'))
        self.assertFalse(any(path == '/api/restore' for path, _ in self.calls))
        self.page.evaluate('nativeCalls.length=0')
        self.login()
        self.usage_extra = {'virtualPlanName': 'PRO Max', 'validUntil': 1}
        self.page.evaluate('loadUsage()')
        self.assertTrue(self.page.evaluate('expired()'))
        self.assertEqual(self.page.evaluate('authorizationInfo.validUntil'), 1)
        self.usage_extra['validUntil'] = 2000000000
        self.page.evaluate('loadUsage()')
        self.assertFalse(self.page.evaluate('expired()'))
        expect(self.page.locator('#plan-summary')).to_contain_text('PRO Max /')
        self.page.locator('#takeover').click(); self.confirm()
        self.page.locator('#window-close').click()
        expect(self.page.locator('#confirm-submit')).to_have_text('还原并退出')
        self.page.keyboard.press('Escape')
        self.assertFalse(any(c[0] in ['close', 'minimize'] for c in self.page.evaluate('nativeCalls')))
        self.fail_restore = True
        self.page.locator('#window-close').click(); self.confirm()
        expect(self.page.locator('#restore-failed')).to_be_visible()
        self.assertFalse(any(c[0] == 'close' for c in self.page.evaluate('nativeCalls')))
        self.fail_restore = False
        self.page.locator('#window-close').click(); self.confirm()
        self.assertIn(['close', 'local-test-token'], self.page.evaluate('nativeCalls'))

    def test_diagnostics_redacted_report_export_and_back(self):
        self.login(); self.page.locator('nav [data-view=doctor]').click(); self.page.locator('#run-doctor').click()
        expect(self.page.locator('#doctor-title')).to_have_text('连接需要检查'); self.shot('05-diagnostic')
        self.page.locator('#preview-report').click(); self.shot('13-report')
        report = self.page.locator('#report-content').inner_text()
        for secret in ['private','secret','10.0.0.1','Bearer','<script>','local-test-token','test-card-secret']:
            self.assertNotIn(secret, report)
        with self.page.expect_download() as download:
            self.page.locator('#export-report').click()
        self.assertEqual(download.value.suggested_filename,'Superkiro-diagnostic.txt')
        self.page.locator('#report [data-view=doctor]').click()
        expect(self.page.locator('#doctor')).to_be_visible()
        self.assertEqual(self.page.locator('nav button').count(),4)

    def test_native_platform_settings_missing_install_and_memory(self):
        self.login(); self.state['platform'] = 'darwin'; self.page.evaluate('status()')
        self.page.locator('nav [data-view=settings]').click()
        expect(self.page.locator('#memory-value')).to_contain_text('1,240 MB')
        self.shot('06-settings-mac')
        self.assertEqual(self.page.locator('#window-maximize').is_visible(),True)
        self.assertLess(self.page.locator('.window-controls').bounding_box()['x'],self.page.locator('.brand').bounding_box()['x'])
        self.page.locator('#window-maximize').click(); self.page.locator('#window-minimize').click()
        calls = self.page.evaluate('nativeCalls')
        self.assertIn(['maximize','local-test-token'],calls)
        self.assertIn(['minimize','local-test-token'],calls)
        self.assertFalse(any(c[0]=='close' for c in calls))
        self.assertIn('系统管理',self.page.locator('#maintenance-state').inner_text())
        self.assertTrue(self.page.locator('#trim-memory').is_disabled())
        self.assertIn('C:/Users/private/Kiro.exe',self.page.locator('#install-path').inner_text())
        self.state['has_snapshot'] = True
        self.page.evaluate('status()')
        self.assertTrue(self.page.locator('#settings [data-action=pick-install]').is_disabled())
        self.state['has_snapshot'] = False
        self.state['kiro_installed'] = False
        self.page.evaluate("status().then(()=>navigate('status'))")
        expect(self.page.locator('#missing')).to_be_visible(); self.shot('07-missing')
        self.page.locator('#missing [data-action=pick-install]').click()
        self.page.wait_for_function('nativeCalls.some(c=>c[0]==="pick_install_path")')
        self.assertIn(['pick_install_path','local-test-token'],self.page.evaluate('nativeCalls'))

    def test_failure_expiry_restore_and_safe_cancel(self):
        self.fail_login = True
        self.page.set_viewport_size({'width':480,'height':670})
        self.page.locator('#card-input-field').fill('invalid'); self.page.locator('#login-submit').click()
        expect(self.page.locator('#login-error')).to_be_visible(); self.shot('11-login-failure')
        self.assertNotIn('SECRET',self.page.locator('#login-error').inner_text())
        self.fail_login = False; self.login(); self.page.set_viewport_size({'width':620,'height':820})
        self.page.evaluate("authorizationInfo.isExpired=true;navigate('status')")
        expect(self.page.locator('#expired')).to_be_visible(); self.shot('09-expired')
        self.fail_restore = True
        self.page.locator('#expired [data-action=restore]').click(); self.confirm()
        expect(self.page.locator('#restore-failed')).to_be_visible(); self.shot('10-restore-failed')
        self.assertFalse(any(c[0]=='close' for c in self.page.evaluate('nativeCalls')))
        self.page.locator('#restore-failed [data-view=doctor]').click()
        expect(self.page.locator('#doctor')).to_be_visible()

    def test_connecting_timeout_and_navigation(self):
        self.login()
        # Pending request is held locally: do not invent a native step or hit production.
        pending = []
        self.page.route('**/api/activate', lambda route: pending.append(route))
        self.page.locator('#takeover').click(); self.page.locator('#confirm-submit').click()
        expect(self.page.locator('#connecting')).to_be_visible(); self.shot('08-connecting')
        self.assertIn('等待桥接结果',self.page.locator('#connection-steps').inner_text())
        self.assertTrue(self.page.locator('#takeover').is_disabled())
        self.page.locator('nav [data-view=settings]').click()
        self.page.locator('nav [data-view=status]').click()
        expect(self.page.locator('#connecting')).to_be_visible()
        pending[0].abort('timedout')
        expect(self.page.locator('#doctor')).to_be_visible()
        self.page.wait_for_function('!busy')
        self.assertTrue(self.page.evaluate('mutationUncertain'))

    def test_native_keyring_only_saves_after_successful_checked_login(self):
        self.page.evaluate("""window.keyringCalls=[]; Object.assign(window.pywebview.api, {
            get_remembered_card:async token=>null,
            set_remembered_card:async (card,token)=>{keyringCalls.push(['set',card,token]);return true},
            clear_remembered_card:async token=>{keyringCalls.push(['clear',token]);return true}
        }); window.dispatchEvent(new Event('pywebviewready'));""")
        expect(self.page.locator('#remember-card')).to_be_enabled()
        self.page.locator('#remember-card').check()
        self.fail_login = True
        self.page.locator('#card-input-field').fill('invalid-card'); self.page.locator('#login-submit').click()
        expect(self.page.locator('#login-error')).to_be_visible()
        self.assertEqual(self.page.evaluate('keyringCalls'), [])
        self.fail_login = False; self.login()
        self.assertEqual(self.page.evaluate('keyringCalls'), [['set','test-card-secret','local-test-token']])
        self.assertNotIn('test-card-secret',self.page.evaluate('JSON.stringify({...localStorage,...sessionStorage})'))
        self.page.evaluate("view('connect')")
        self.page.locator('#remember-card').uncheck()
        self.page.wait_for_function('!credentialWritePending')
        self.assertEqual(self.page.evaluate('keyringCalls.at(-1)'), ['clear','local-test-token'])

    def test_native_keyring_load_does_not_activate_and_linux_trim_disabled(self):
        self.page.evaluate("""Object.assign(window.pywebview.api, {
            get_remembered_card:async token=>'os-keyring-secret',
            set_remembered_card:async ()=>true, clear_remembered_card:async ()=>true
        });window.dispatchEvent(new Event('pywebviewready'));""")
        expect(self.page.locator('#card-input-field')).to_have_value('os-keyring-secret')
        self.assertFalse(any(path in ['/api/verify-card','/api/activate'] for path,_ in self.calls))
        self.state['platform'] = 'linux'; self.page.evaluate('status()')
        self.page.evaluate("view('settings')")
        expect(self.page.locator('#trim-memory')).to_be_disabled()
        expect(self.page.locator('#maintenance-state')).to_have_text('内存由系统管理')
        self.page.evaluate("document.getElementById('trim-memory').onclick()")
        self.assertFalse(any(path == '/api/memory/trim' for path,_ in self.calls))

    def test_keyring_failure_and_switch_cleanup(self):
        self.page.evaluate("""window.keyringCalls=[]; Object.assign(window.pywebview.api, {
            get_remembered_card:async ()=>null, set_remembered_card:async ()=>false,
            clear_remembered_card:async token=>{keyringCalls.push(['clear',token]);return false}
        });window.dispatchEvent(new Event('pywebviewready'));""")
        expect(self.page.locator('#remember-card')).to_be_enabled()
        self.page.locator('#remember-card').check(); self.login()
        expect(self.page.locator('#message')).to_contain_text('系统安全存储保存失败')
        self.page.locator('nav [data-view=settings]').click()
        self.page.locator('#settings [data-action=switch]').click(); self.confirm()
        expect(self.page.locator('#connect')).to_be_visible()
        expect(self.page.locator('#message')).to_contain_text('未能清除')
        self.assertEqual(self.page.evaluate('keyringCalls'),[['clear','local-test-token']])
        self.page.locator('#remember-card').click()
        self.page.wait_for_function('!credentialWritePending')
        expect(self.page.locator('#remember-card')).to_be_checked()
        expect(self.page.locator('#message')).to_contain_text('无法删除')

    def test_external_browser_native_first_failure_and_preview_fallback(self):
        self.state['portal_url'] = 'https://gateway.invalid/portal'
        self.page.evaluate("async () => { await status(); window.previewCalls=[]; window.open=(...a)=>window.previewCalls.push(a); window.pywebview.api.open_external=(...a)=>{window.nativeCalls.push(['open_external',...a]);return true;}; }")
        self.page.locator('#connect [data-action="portal"]').click()
        self.page.evaluate("() => openExternal('https://kiro.dev/downloads/')")
        calls = self.page.evaluate("window.nativeCalls.filter(c=>c[0]==='open_external')")
        self.assertEqual(calls, [['open_external','https://gateway.invalid/portal','local-test-token'], ['open_external','https://kiro.dev/downloads/','local-test-token']])
        self.assertEqual(self.page.evaluate('window.previewCalls'), [])
        self.page.evaluate("async () => { window.pywebview.api.open_external=()=>false; await openExternal('https://gateway.invalid/portal'); }")
        expect(self.page.locator('#message')).to_have_text('无法打开系统浏览器，请稍后重试。')
        self.page.evaluate("async () => { window.pywebview.api.open_external=()=>{throw Error('private detail')}; await openExternal('https://gateway.invalid/portal'); }")
        self.assertEqual(self.page.evaluate('window.previewCalls'), [])
        self.page.evaluate("async () => { delete window.pywebview.api.open_external; await openExternal('https://other.invalid/'); }")
        expect(self.page.locator('#message')).to_have_text('外部地址未通过安全检查。')
        self.assertEqual(self.page.evaluate('window.previewCalls'), [])
        self.page.evaluate("() => openExternal('https://gateway.invalid/portal')")
        self.assertEqual(self.page.evaluate('window.previewCalls'), [['https://gateway.invalid/portal','_blank','noopener,noreferrer']])

    def test_responsive_keyboard_and_markup(self):
        self.login()
        for width,height in [(320,480),(480,620),(620,820),(780,900)]:
            self.page.set_viewport_size({'width':width,'height':height})
            for name in ['connect','status','usage','doctor','settings','report','missing','connecting','expired','restore-failed']:
                self.page.evaluate('(name)=>view(name)',name)
                self.assertTrue(self.page.evaluate('document.documentElement.scrollWidth <= innerWidth'),(width,name))
        ids = self.page.locator('[id]').evaluate_all('(els)=>els.map(e=>e.id)')
        self.assertEqual(len(ids),len(set(ids)))
        self.assertFalse(self.page.locator('img').count())  # References are not flattened into the UI.
        self.page.evaluate("view('status')")
        self.page.locator('#takeover').click()
        expect(self.page.locator('#confirm-cancel')).to_be_focused()
        for _ in range(5): self.page.keyboard.press('Tab')
        self.assertTrue(self.page.evaluate('document.getElementById("confirm-dialog").contains(document.activeElement)'))
        self.page.keyboard.press('Escape')
        expect(self.page.locator('#confirm-dialog')).not_to_be_visible()


if __name__ == '__main__':
    unittest.main(verbosity=2)
