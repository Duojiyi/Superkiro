"""Pencil geometry and state screenshots using the existing offline portal fixture."""
import sys
import unittest
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from test_superkiro_portal_ui import PortalBrowserTests, CARD
from playwright.sync_api import expect


class PencilAlignmentTests(PortalBrowserTests):
    def test_pencil_desktop(self):
        self.goto('/')
        self.page.evaluate('document.fonts.ready')
        expect(self.page.locator('#release-status')).to_contain_text('暂无可用发布')
        stage = self.page.locator('.stage').bounding_box()
        self.assertEqual(stage, dict(x=752, y=164, width=624, height=432))
        self.assertEqual(self.page.locator('.hero').bounding_box()['height'], 576)
        self.assertEqual(self.page.locator('.quick-plan dt').first.evaluate('(e)=>getComputedStyle(e).fontSize'), '60px')
        self.assertEqual(self.page.locator('.quick-price').first.evaluate('(e)=>getComputedStyle(e).fontSize'), '40px')
        first = self.page.locator('.quick-plan').first
        self.assertGreater(first.locator('.quick-points').bounding_box()['y'], first.locator('dt').bounding_box()['y'])
        for row in self.page.locator('.quick-plan').all()[1:]:
            a, b, c = [row.locator(s).bounding_box() for s in ['dt', '.quick-points', '.quick-price']]
            self.assertLess(a['x'], b['x'])
            self.assertLess(b['x'], c['x'])
            self.assertLess(abs(a['y']-c['y']), 3)
        self.screenshot('pencil-home-desktop.png')
        sections = self.page.locator('.header,.hero,.steps,.pricing,.downloads,.footer').evaluate_all('(els)=>els.map(e=>({section:e.className,y:e.getBoundingClientRect().y,height:e.getBoundingClientRect().height}))')
        (Path(__file__).parent/'test-artifacts/pencil-desktop-measurements.json').write_text(__import__('json').dumps(sections, indent=2))
        self.page.locator('.stage').screenshot(path=str(Path(__file__).parent/'test-artifacts/pencil-pro-card.png'))
        self.assertTrue(all(url.startswith(self.origin) for url in self.requests))

    def test_pencil_mobile(self):
        self.page.set_viewport_size(dict(width=375, height=812))
        self.goto('/')
        self.page.evaluate('document.fonts.ready')
        expect(self.page.locator('.stage')).to_be_hidden()
        self.assertEqual(self.page.locator('.hero').bounding_box()['x'], 24)
        self.assertEqual(self.page.locator('.hero-copy').bounding_box()['x'], 24)
        self.assertEqual(self.page.locator('.hero-actions .primary').bounding_box()['width'], 327)
        self.assertEqual(self.page.locator('.header').bounding_box()['height'], 72)
        for card in self.page.locator('.plan-card').all():
            name, points, price = [card.locator(s).bounding_box() for s in ['h3','.plan-points','.plan-price']]
            self.assertEqual(name['x'], points['x'])
            self.assertEqual(name['x'], price['x'])
            self.assertLess(points['y'], price['y'])
        self.assertTrue(self.page.evaluate('document.documentElement.scrollWidth <= innerWidth'))
        self.screenshot('pencil-home-mobile.png')

    def test_pencil_device_states(self):
        self.goto('/device')
        self.page.evaluate('document.fonts.ready')
        self.screenshot('pencil-device-empty.png')
        self.verify()
        expect(self.page.locator('#verify-form')).to_be_hidden()
        expect(self.page.locator('#account-details')).to_contain_text('1842.5 积分')
        self.screenshot('pencil-device-verified.png')
        self.page.set_viewport_size(dict(width=375, height=812))
        self.screenshot('pencil-device-verified-mobile.png')
        self.page.set_viewport_size(dict(width=1440, height=1000))
        self.page.locator('#request-unbind').click()
        expect(self.page.locator('[data-close="confirm-dialog"]')).to_be_focused()
        self.screenshot('pencil-device-confirm.png')
        self.page.locator('#confirm-unbind').click()
        expect(self.page.locator('#recovery')).to_be_visible()
        expect(self.page.locator('#recovery')).to_be_focused()
        self.screenshot('pencil-device-success.png')
        self.assertTrue(self.page.evaluate('localStorage.length === 0 && sessionStorage.length === 0'))
        self.page.set_viewport_size(dict(width=375, height=812))
        self.goto('/device')
        self.fail_action = 'query'
        self.page.locator('#card').fill(CARD)
        self.page.locator('#verify').click()
        expect(self.page.locator('#device-message')).to_have_class('message error')
        self.screenshot('pencil-device-error-mobile.png')


    def test_pencil_device_reverification_and_fonts(self):
        self.goto('/device')
        self.query['virtualPlanName'] = '动态授权套餐'
        self.verify()
        expect(self.page.locator('#verify-form')).to_be_hidden()
        expect(self.page.locator('#account')).to_be_focused()
        expect(self.page.locator('#account-details')).to_contain_text('1842.5 积分')
        self.assertEqual(self.page.locator('#card').input_value(), '')
        self.page.evaluate('document.fonts.ready')
        cdp = self.context.new_cdp_session(self.page)
        cdp.send('DOM.enable')
        cdp.send('CSS.enable')
        root_id = cdp.send('DOM.getDocument')['root']['nodeId']
        fonts = []
        nodes = cdp.send('DOM.querySelectorAll', {'nodeId': root_id, 'selector': '#plan, #account-details dt, #account-details dd'})['nodeIds']
        for node_id in nodes:
            rendered = cdp.send('CSS.getPlatformFontsForNode', {'nodeId': node_id})['fonts']
            self.assertTrue(rendered)
            fonts.extend(rendered)
        self.assertTrue(fonts)
        for font in fonts:
            family = font['familyName'].lower()
            self.assertFalse(any(name in family for name in ['serif', 'simsun', '宋体']), font)
            self.assertTrue(any(name in family for name in [
                'yahei', 'pingfang', 'noto sans', 'arial',
                'liberation sans', 'dejavu sans', 'wenquanyi zen hei',
            ]), font)
        (Path(__file__).parent/'test-artifacts/pencil-device-fonts.json').write_text(__import__('json').dumps(fonts, ensure_ascii=False, indent=2), encoding='utf-8')
        for balance, expected in [(0, '0 积分'), (None, '未提供')]:
            self.page.locator('#reset-card').click()
            expect(self.page.locator('#verify-form')).to_be_visible()
            expect(self.page.locator('#card')).to_be_focused()
            expect(self.page.locator('#account')).to_be_hidden()
            self.query['remainingPoints'] = balance
            self.verify()
            expect(self.page.locator('#account-details > div').nth(1).locator('dd')).to_have_text(expected)
        self.fail_action = 'challenge'
        self.page.locator('#request-unbind').click()
        self.page.locator('#confirm-unbind').click()
        expect(self.page.locator('#verify-form')).to_be_visible()
        expect(self.page.locator('#card')).to_be_focused()
        self.fail_action = None
        self.verify()
        expect(self.page.locator('#verify-form')).to_be_hidden()
        self.assertEqual(len([action for action, _ in self.calls if action == 'query']), 4)


def load_tests(loader, tests, pattern):
    return unittest.TestSuite(PencilAlignmentTests(name) for name in loader.getTestCaseNames(PencilAlignmentTests) if name.startswith('test_pencil_'))


if __name__ == '__main__':
    unittest.main()
