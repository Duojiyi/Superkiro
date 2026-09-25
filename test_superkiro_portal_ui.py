"""Offline browser contracts for the native portal; no production cards or downloads."""
import json
import re
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from playwright.sync_api import sync_playwright, expect

ROOT = Path(__file__).resolve().parent
UI = ROOT / 'apps' / 'portal-ui'
CARD = 'browser-test-card-ONLY'
DEVICE = 'fixture-device-private-AB12'


class StaticHandler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        path = self.path.split('?')[0]
        if path in ('/', '/portal', '/portal/') or path.startswith(('/device', '/docs')):
            data = (UI / 'index.html').read_bytes()
            self.send_response(200)
            self.send_header('Content-Type', 'text/html; charset=utf-8')
            # The policy production serves outside /admin, which this page runs under.
            policy = re.findall(r'header @not_admin_console Content-Security-Policy "([^"]+)"',
                                (ROOT / 'deploy/Caddyfile.ip').read_text())[0]
            self.send_header('Content-Security-Policy', policy)
            self.end_headers()
            self.wfile.write(data)
        else:
            self.send_error(404)


class PortalBrowserTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = ThreadingHTTPServer(('127.0.0.1', 0), StaticHandler)
        cls.worker = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.worker.start()
        cls.origin = f'http://127.0.0.1:{cls.server.server_port}'
        cls.pw = sync_playwright().start()
        cls.browser = cls.pw.chromium.launch()

    @classmethod
    def tearDownClass(cls):
        cls.browser.close()
        cls.pw.stop()
        cls.server.shutdown()
        cls.server.server_close()
        cls.worker.join(5)

    def setUp(self):
        self.context = self.browser.new_context(viewport={'width': 1440, 'height': 1000})
        self.page = self.context.new_page()
        self.errors = []
        self.calls = []
        self.requests = []
        self.manifest = {'releases': []}
        self.manifest_status = 200
        self.fail_action = None
        self.fail_status = 400
        self.fail_headers = {'Retry-After': '30'}
        self.fail_payload = {}
        self.bad_challenge = False
        self.bad_unbind = False
        self.query = dict(success=True, status='active', remainingPoints=1842.5,
                          validUntil=1900000000, isExpired=False,
                          boundDevices=[DEVICE], virtualPlanName='PRO+',
                          rebindCount=0, maxRebinds=5)
        self.page.on('pageerror', lambda error: self.errors.append(str(error)))
        self.page.on('request', lambda request: self.requests.append(request.url))
        self.page.route('**/downloads/releases.json', self.release_response)
        self.page.route('**/api/v1/portal/*', self.api_response)

    def tearDown(self):
        self.context.close()
        self.assertEqual(self.errors, [])

    def release_response(self, route):
        route.fulfill(status=self.manifest_status, json=self.manifest)

    def api_response(self, route):
        action = route.request.url.rsplit('/', 1)[-1]
        body = route.request.post_data_json
        self.calls.append((action, body))
        self.assertEqual(route.request.method, 'POST')
        self.assertNotIn(CARD, route.request.url)
        if action == self.fail_action:
            route.fulfill(status=self.fail_status, headers=self.fail_headers,
                          json={'success': False, 'error': CARD + DEVICE, **self.fail_payload})
        elif action == 'query':
            route.fulfill(json=self.query)
        elif action == 'challenge':
            route.fulfill(json={'success': True, 'challengeToken': '' if self.bad_challenge else 'fixture-onetime-token', 'expiresIn': 120})
        elif action == 'unbind':
            route.fulfill(json={'success': True, 'remainingDevices': [DEVICE] if self.bad_unbind else []})
        else:
            self.fail('Unexpected mutating or binding API: ' + action)

    def goto(self, path='/'):
        self.page.goto(self.origin + path)

    def verify(self):
        self.page.locator('#card').fill(CARD)
        self.page.locator('#verify').click()
        expect(self.page.locator('#account')).to_be_visible()

    def assert_disabled(self, key):
        for anchor in self.page.locator(f'[data-download="{key}"]').all():
            self.assertEqual(anchor.get_attribute('aria-disabled'), 'true')
            self.assertIsNone(anchor.get_attribute('href'))

    def screenshot(self, name):
        target = UI / 'test-artifacts'
        target.mkdir(exist_ok=True)
        self.page.evaluate('document.activeElement.blur()')
        self.page.screenshot(path=str(target / name), full_page=True, animations='disabled')

    def test_01_readonly_verify_and_confirmed_unbind(self):
        self.goto('/device')
        self.verify()
        self.assertEqual(self.calls, [('query', {'card': CARD})])
        self.assertEqual(self.page.locator('#card').input_value(), '')
        self.assertNotIn(CARD, self.page.content())
        self.assertNotIn(DEVICE, self.page.content())
        expect(self.page.locator('#bound-device')).to_contain_text('•••• AB12')
        self.screenshot('device-desktop.png')
        self.page.locator('#request-unbind').click()
        expect(self.page.locator('[data-close="confirm-dialog"]')).to_be_focused()
        self.page.keyboard.press('Escape')
        expect(self.page.locator('#request-unbind')).to_be_focused()
        self.assertEqual(len(self.calls), 1)
        self.page.locator('#request-unbind').click()
        self.page.locator('#confirm-unbind').click()
        expect(self.page.locator('#device-message')).to_contain_text('旧设备已解绑')
        self.assertEqual(self.calls[1:], [('challenge', {'action': 'unbind'}),
                         ('unbind', {'card': CARD, 'device': DEVICE, 'challenge_token': 'fixture-onetime-token'})])
        expect(self.page.locator('#account')).to_be_hidden()
        expect(self.page.locator('#recovery')).to_be_visible()
        self.assertTrue(self.page.evaluate('localStorage.length === 0 && sessionStorage.length === 0'))
        self.assertEqual(self.context.cookies(), [])
        self.assertTrue(all(CARD not in url and DEVICE not in url for url in self.requests))

    def test_02_empty_and_failed_releases_remain_disabled(self):
        self.goto()
        expect(self.page.locator('#release-status')).to_contain_text('暂无可用发布')
        for key in ['windows-x64', 'macos-arm64', 'macos-x64']:
            self.assert_disabled(key)
        self.page.locator('[data-mac]').first.click()
        expect(self.page.locator('#mac-dialog')).to_be_visible()
        expect(self.page.locator('#mac-dialog')).to_contain_text('尚未签名与公证')
        self.page.keyboard.press('Escape')
        self.manifest_status = 404
        self.page.locator('#retry-releases').click()
        expect(self.page.locator('#release-status')).to_contain_text('暂不可用')
        self.assert_disabled('windows-x64')

    def test_03_valid_and_invalid_release_metadata(self):
        release = dict(platform='windows', arch='x64', version='1.2.3-test',
                       url='/downloads/fixture-win.exe', sha256='ab' * 32,
                       size=10485760, systemRequirements='Windows 测试要求', signature='unsigned')
        self.manifest = {'releases': [release,
                         dict(release, platform='macos', arch='arm64', url='/downloads/fixture-arm.dmg'),
                         dict(release, platform='macos', url='javascript:alert(1)')]}
        self.goto()
        expect(self.page.locator('#release-status')).to_contain_text('发布信息已更新')
        for a in self.page.locator('[data-download="windows-x64"]').all():
            self.assertEqual(a.get_attribute('href'), self.origin + '/downloads/fixture-win.exe')
        expect(self.page.locator('[data-release="windows-x64"]')).to_contain_text('10.0 MB')
        expect(self.page.locator('[data-release="windows-x64"]')).to_contain_text('ab' * 32)
        self.assert_disabled('macos-x64')
        # A failing refresh must revoke stale links, not leave old downloads enabled.
        self.manifest_status = 503
        self.page.locator('#retry-releases').click()
        expect(self.page.locator('#release-status')).to_contain_text('暂不可用')
        self.assert_disabled('windows-x64')
        self.manifest_status = 200
        for entries in [[release, release], [dict(release, sha256='invalid')],
                        [dict(release, size=0)], [dict(release, url='https://github.com/org/private/actions/artifacts/1')],
                        [dict(release, url='https://user:password@example.com/file')],
                        [dict(release, url='http://example.com/file')], [dict(release, systemRequirements='')]]:
            self.manifest = {'releases': entries}
            self.page.locator('#retry-releases').click()
            expect(self.page.locator('#release-status')).to_contain_text('暂无可用发布')
            self.assert_disabled('windows-x64')

    def test_03b_unusable_card_offers_no_unbind_and_another_card_can_follow(self):
        self.goto('/device')
        # The customer site never advertises the administrator console.
        self.assertEqual(self.page.locator('a[href="/admin/"]').count(), 0)
        for status, label in (('voided', '已删除（作废）'), ('banned', '已禁用')):
            self.query['status'] = status
            self.verify()
            expect(self.page.locator('#account-details')).to_contain_text(label)
            expect(self.page.locator('#masked-card')).to_contain_text(CARD[-4:])
            self.assertTrue(self.page.locator('#request-unbind').is_disabled())
            expect(self.page.locator('#device-message')).to_contain_text('不提供解绑')
            self.page.locator('#reset-card').click()
        self.query['status'] = 'active'
        self.verify()
        self.assertFalse(self.page.locator('#request-unbind').is_disabled())
        self.page.locator('#request-unbind').click()
        self.page.locator('#confirm-unbind').click()
        expect(self.page.locator('#recovery')).to_be_visible()
        self.page.locator('#another-card').click()
        expect(self.page.locator('#verify-form')).to_be_visible()
        expect(self.page.locator('#masked-card')).to_have_text('')

    def test_04_query_failures_and_untrusted_text(self):
        self.goto('/device')
        # A lockout's own wait is shown; with none given, the page must not invent one.
        for status, headers, text in [(400, {}, '验证未通过'), (429, {'Retry-After': '30'}, '30 秒'),
                                      (429, {'Retry-After': '900'}, '15 分钟'), (429, {}, '等待时间未知'),
                                      (404, {}, '尚未开放'), (503, {}, '验证未通过')]:
            self.fail_action, self.fail_status, self.fail_headers = 'query', status, headers
            self.page.locator('#card').fill(CARD)
            self.page.locator('#verify').click()
            expect(self.page.locator('#device-message')).to_contain_text(text)
            expect(self.page.locator('#account')).to_be_hidden()
            self.assertNotIn(CARD, self.page.locator('#device-message').inner_text())
        self.fail_action = None
        self.query['virtualPlanName'] = '<img src=x onerror=alert(1)>'
        self.verify()
        expect(self.page.locator('#plan')).to_have_text('<img src=x onerror=alert(1)>')
        self.assertEqual(self.page.locator('#plan img').count(), 0)
        self.page.locator('#reset-card').click()
        self.assertEqual(self.page.locator('#plan').inner_text(), '')

    def test_05_unbind_failures_require_reverification(self):
        self.goto('/device')
        for action, status in [('challenge', 429), ('unbind', 403), ('unbind', 400), ('unbind', 503)]:
            self.fail_action = None
            self.verify()
            self.fail_action, self.fail_status = action, status
            self.page.locator('#request-unbind').click()
            self.page.locator('#confirm-unbind').click()
            expect(self.page.locator('#account')).to_be_hidden()
            expect(self.page.locator('#device-message')).to_contain_text('重新验证')
            self.assertNotIn(DEVICE, self.page.locator('#device-message').inner_text())
            expect(self.page.locator('#recovery')).to_be_hidden()
        self.assertEqual(len([a for a, _ in self.calls if a == 'challenge']), 4)

    def test_06_missing_challenge_and_false_success(self):
        self.goto('/device')
        self.verify()
        self.bad_challenge = True
        self.page.locator('#request-unbind').click()
        self.page.locator('#confirm-unbind').click()
        expect(self.page.locator('#device-message')).to_contain_text('未获取操作凭证')
        self.assertFalse(any(a == 'unbind' for a, _ in self.calls))
        self.bad_challenge = False
        self.bad_unbind = True
        self.verify()
        self.page.locator('#request-unbind').click()
        self.page.locator('#confirm-unbind').click()
        expect(self.page.locator('#device-message')).to_contain_text('未确认设备已解绑')
        expect(self.page.locator('#recovery')).to_be_hidden()

    def test_07_unbound_expired_multiple_devices(self):
        self.query.update(boundDevices=[], status='expired', isExpired=True)
        self.goto('/device')
        self.verify()
        expect(self.page.locator('#account-details')).to_contain_text('已过期')
        expect(self.page.locator('#request-unbind')).to_be_disabled()
        self.page.locator('#reset-card').click()
        self.query['boundDevices'] = [DEVICE, 'second-real-fixture-CD34']
        self.verify()
        self.page.locator('#bound-device').select_option('1')
        self.page.locator('#request-unbind').click()
        expect(self.page.locator('#confirm-device')).to_have_text('设备：•••• CD34')
        self.page.locator('#confirm-unbind').click()
        expect(self.page.locator('#device-message')).to_contain_text('旧设备已解绑')
        self.assertEqual(self.calls[-1][1]['device'], 'second-real-fixture-CD34')

    def test_08_routes_docs_and_mobile_layout(self):
        for path, heading in [('/', '更多可能。'), ('/portal/', '更多可能。'),
                              ('/device/', '换机管理'), ('/device/manage', '换机管理'),
                              ('/docs', '第一次使用 Superkiro'), ('/docs/restore', '恢复原始配置')]:
            self.goto(path)
            expect(self.page.locator('h1:visible')).to_contain_text(heading)
        self.goto('/docs')
        self.page.get_by_role('button', name='macOS', exact=True).click()
        expect(self.page.locator('[data-platform="macos"]')).to_have_attribute('aria-pressed', 'true')
        expect(self.page.locator('#platform-help')).to_contain_text('尚未签名与公证')
        for name in ['models', 'device', 'restore', 'connection', 'downloads', 'start']:
            self.page.locator(f'.docs-nav a[href="#{name}"]').click()
            expect(self.page.locator(f'[data-doc="{name}"]')).to_be_visible()
        self.screenshot('docs-desktop.png')
        for width in [320, 375, 768, 1440]:
            self.page.set_viewport_size({'width': width, 'height': 900})
            for path in ['/', '/device', '/docs', '/docs#connection']:
                self.goto(path)
                self.assertTrue(self.page.evaluate('document.documentElement.scrollWidth <= innerWidth'), (width, path))
                self.assertEqual(self.page.locator('h1:visible').count(), 1)
                self.assertTrue(all(a.get_attribute('href') == '/admin/' for a in self.page.locator('a').all() if '管理入口' in a.inner_text()))
        self.page.set_viewport_size({'width': 375, 'height': 812})
        self.goto('/')
        self.page.locator('.menu').click()
        expect(self.page.locator('.menu')).to_have_attribute('aria-expanded', 'true')
        expect(self.page.locator('#site-nav')).to_be_visible()
        self.page.locator('.menu').click()
        self.screenshot('home-mobile.png')
        self.goto('/device')
        self.verify()
        self.screenshot('device-mobile.png')
        self.page.locator('#request-unbind').click()
        self.screenshot('confirm-mobile.png')
        self.assertTrue(self.page.evaluate('document.documentElement.scrollWidth <= innerWidth'))

    def test_09_keyboard_motion_and_no_external_dependencies(self):
        self.page.emulate_media(reduced_motion='reduce')
        self.goto('/')
        expect(self.page.locator('#release-status')).to_contain_text('暂无可用发布')
        self.page.keyboard.press('Tab')
        expect(self.page.locator('.skip')).to_be_focused()
        self.page.keyboard.press('Enter')
        expect(self.page.locator('#main')).to_be_focused()
        self.assertEqual(self.page.locator('.pulse').first.evaluate('(el)=>getComputedStyle(el).animationName'), 'none')
        self.assertFalse(self.page.locator('.stage').evaluate("el=>el.classList.contains('running')"))
        self.screenshot('home-desktop.png')
        self.page.locator('[data-mac]').first.click()
        expect(self.page.locator('[data-close="mac-dialog"]')).to_be_focused()
        self.page.keyboard.press('Shift+Tab')
        self.assertTrue(self.page.evaluate("document.querySelector('#mac-dialog').contains(document.activeElement)"))
        self.page.keyboard.press('Escape')
        expect(self.page.locator('[data-mac]').first).to_be_focused()
        self.assertTrue(all(url.startswith(self.origin) for url in self.requests))
        # Keep the 250ms motion state deterministic on loaded CI runners.
        self.page.locator('.stage').scroll_into_view_if_needed()
        self.page.clock.install()
        self.page.clock.pause_at(self.page.evaluate('Date.now() + 1000'))
        self.page.emulate_media(reduced_motion='no-preference')
        expect(self.page.locator('.stage')).to_have_class('stage running')
        self.page.locator('.footer').scroll_into_view_if_needed()
        expect(self.page.locator('.stage')).to_have_class('stage')

    def test_10_insecure_origin_and_network_failures(self):
        self.page.route('http://portal.invalid/device', lambda r: r.fulfill(content_type='text/html', body=(UI/'index.html').read_text(encoding='utf-8')))
        self.page.goto('http://portal.invalid/device')
        self.page.locator('#card').fill(CARD)
        self.page.locator('#verify').click()
        expect(self.page.locator('#device-message')).to_contain_text('HTTPS')
        self.assertEqual(self.calls, [])
        self.goto('/device')
        self.page.route('**/api/v1/portal/query', lambda r: r.abort('failed'))
        self.page.locator('#card').fill(CARD)
        self.page.locator('#verify').click()
        expect(self.page.locator('#device-message')).to_contain_text('网络中断')
        expect(self.page.locator('#verify')).to_be_enabled()


    def test_11_duplicate_submits_do_not_repeat_requests(self):
        self.goto('/device')
        self.page.locator('#card').fill(CARD)
        self.page.evaluate("""()=>{const f=document.querySelector('#verify-form'); f.requestSubmit(); f.requestSubmit();}""")
        expect(self.page.locator('#account')).to_be_visible()
        self.assertEqual(len(self.calls), 1)
        self.page.locator('#request-unbind').click()
        self.page.evaluate("""()=>{const b=document.querySelector('#confirm-unbind'); b.click(); b.click();}""")
        expect(self.page.locator('#device-message')).to_contain_text('旧设备已解绑')
        self.assertEqual([a for a, _ in self.calls], ['query', 'challenge', 'unbind'])

    def test_12_text_token_contrast(self):
        self.goto('/')
        tokens = self.page.evaluate("""()=>{const s=getComputedStyle(document.documentElement);return Object.fromEntries(['bg','surface','text','muted','silver','error'].map(k=>[k,s.getPropertyValue('--'+k).trim()]));}""")
        def luminance(hex_color):
            rgb = [int(hex_color[i:i+2], 16)/255 for i in (1, 3, 5)]
            linear = [x/12.92 if x <= .04045 else ((x+.055)/1.055)**2.4 for x in rgb]
            return sum(x*w for x,w in zip(linear, [.2126, .7152, .0722]))
        for fg, bg in [('text', 'bg'), ('muted', 'bg'), ('text', 'surface'), ('muted', 'surface'), ('error', 'bg'), ('bg', 'silver')]:
            light, dark = sorted([luminance(tokens[fg]), luminance(tokens[bg])], reverse=True)
            self.assertGreaterEqual((light+.05)/(dark+.05), 4.5, (fg, bg))

    def test_13_docs_verification_and_activation_timing_contract(self):
        self.goto('/docs/start')
        start = self.page.locator('[data-doc="start"]')
        for platform in ['Windows', 'macOS']:
            with self.subTest(platform=platform):
                self.page.get_by_role('button', name=platform, exact=True).click()
                expect(start).to_be_visible()
                expect(start).to_contain_text('客户端和网页的卡密验证均为只读查询，不激活卡密、不绑定设备，也不会启动未激活卡密的有效期计时')
                expect(start).to_contain_text('手动点击并确认“启用连接”，才会请求服务端激活卡密并按策略绑定当前电脑')
                expect(start).to_contain_text('有效期从服务端成功激活时开始计时，不以 IDE 连接完成或发送第一条消息为起点')
                expect(start).to_contain_text('已激活卡密不会因再次验证或连接而重新计时')
                expect(start).not_to_contain_text('客户端验证会按服务端策略绑定当前电脑')
        self.assertEqual(self.calls, [])

    def test_14_restore_documentation_matches_desktop(self):
        self.goto('/docs/restore')
        section = self.page.locator('[data-doc="restore"]')
        # The client asks Kiro to close and forces it only on a second, explicit consent.
        for text in ['还原 Kiro 配置', '请求 Kiro 关闭', '不会被强制结束', '不会退出登录或解除设备绑定', '启用连接']:
            expect(section).to_contain_text(text)
        self.goto('/docs/downloads')
        expect(self.page.locator('[data-doc="downloads"]')).to_contain_text('单文件客户端')

    def test_15_unactivated_expiry_is_not_reported_as_missing(self):
        self.query.update(status='unactivated', validUntil=None, boundDevices=[])
        self.goto('/device')
        self.verify()
        expect(self.page.locator('#account-details')).to_contain_text('首次启用连接后开始计时')
        expect(self.page.locator('#request-unbind')).to_be_disabled()
        expect(self.page.locator('#verify')).to_have_text('验证卡密')
        self.assertIsNone(self.page.locator('#verify-form').get_attribute('aria-busy'))

    def test_16_pagehide_ignores_late_query_response(self):
        self.goto('/device')
        self.page.evaluate("""()=>{window.fetch=()=>new Promise(resolve=>{window.releaseQuery=()=>resolve(new Response(JSON.stringify({success:true,status:'active',boundDevices:[],remainingPoints:1000}),{status:200}));});}""")
        self.page.locator('#card').fill(CARD)
        self.page.locator('#verify').click()
        self.page.evaluate("""async()=>{dispatchEvent(new Event('pagehide'));window.releaseQuery();await new Promise(r=>setTimeout(r,0));}""")
        expect(self.page.locator('#account')).to_be_hidden()
        expect(self.page.locator('#card')).to_have_value('')
        expect(self.page.locator('#verify')).to_be_enabled()

    def test_19_device_docs_do_not_promise_unconditional_binding(self):
        self.goto('/docs/device')
        section = self.page.locator('[data-doc="device"]')
        expect(section).to_contain_text('新客户端能否启用仍受卡密状态、余额和有效期限制')
        expect(section).not_to_contain_text('可立即在新客户端绑定')

    def test_17_unbind_success_does_not_promise_valid_authorization(self):
        for status in ['frozen', 'expired', 'active']:
            with self.subTest(status=status):
                self.query.update(status=status, isExpired=status == 'expired', remainingPoints=0)
                self.goto('/device')
                self.verify()
                self.page.locator('#request-unbind').click()
                self.page.locator('#confirm-unbind').click()
                message = self.page.locator('#device-message')
                expect(message).to_contain_text('旧设备已解绑')
                expect(message).to_contain_text('取决于卡密状态、余额和有效期')
                expect(message).not_to_contain_text('现在可以')
                expect(self.page.locator('#recovery')).to_contain_text('不会解除冻结')
                self.assertTrue(message.evaluate("el=>getComputedStyle(el).fontFamily").startswith('"Microsoft YaHei"'))

    def test_18_unbind_policy_errors_are_whitelisted(self):
        cases = [
            ({'code': 'rebind_limit_exceeded'}, '换绑次数已用尽'),
            ({'code': 'rebind_cooldown', 'retryAfterSecs': 123}, '请等待 123 秒'),
            ({'code': 'rebind_cooldown', 'retryAfterSecs': CARD}, '剩余时间未确认'),
            ({'code': 'rebind_cooldown', 'retryAfterSecs': -1}, '剩余时间未确认'),
            ({'code': 'rebind_cooldown', 'retryAfterSecs': 1.5}, '剩余时间未确认'),
            ({'code': 'rebind_cooldown', 'retryAfterSecs': 9007199254740992}, '剩余时间未确认'),
            ({'code': 'rebind_cooldown'}, '剩余时间未确认'),
            ({'code': 'unknown_' + CARD}, '验证未通过'),
        ]
        for payload, expected in cases:
            with self.subTest(payload=payload):
                self.fail_action = 'unbind'
                self.fail_payload = payload
                self.goto('/device')
                self.verify()
                self.page.locator('#request-unbind').click()
                self.page.locator('#confirm-unbind').click()
                message = self.page.locator('#device-message')
                expect(message).to_contain_text(expected)
                expect(message).not_to_contain_text(CARD)
                expect(message).not_to_contain_text(DEVICE)
                expect(self.page.locator('#recovery')).to_be_hidden()
                expect(self.page.locator('#verify-form')).to_be_visible()

if __name__ == '__main__':
    unittest.main(verbosity=2)

