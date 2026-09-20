"""Offline regression checks for production browser-auth verification."""
import io
import json
import unittest
from unittest.mock import Mock, patch
from deploy import domain_acceptance, release_candidate


class BrowserSessionContract(unittest.TestCase):
    def test_browser_acceptance_uses_real_form_cookie_and_logout(self):
        # Parse/read only: importing or invoking the acceptance entry point must
        # never be necessary for offline contract checks.
        import ast
        from pathlib import Path
        source = Path(__file__).with_name('domain_browser_acceptance.py').read_text(encoding='utf-8')
        tree = ast.parse(source)
        helper = next(n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == 'verify_admin_browser')
        flow = ast.get_source_segment(source, helper)
        for old in ('http_credentials', 'ADMIN_KEY', '管理员密钥', '保存并验证'):
            self.assertNotIn(old, source)
        for required in ("get_by_label('用户名'", "get_by_label('密码'", "name='登录'", "name='退出'",
                         '__Host-admin_session', "cookie['httpOnly']", "cookie['secure']",
                         "cookie['sameSite'] == 'Strict'", '/api/v1/admin/session',
                         '/api/v1/admin/cards/reveal', '/api/v1/admin/session/revoke',
                         "session['body']['csrfToken']", 'x-csrf-token',
                         "reveal['body']['rawCode'] == card['rawCode']",
                         'replay.add_cookies(cookies)',
                         "replay.request.get(BASE + '/api/v1/admin/cards').status == 401"):
            self.assertIn(required, flow)
        self.assertIn('verify_admin_browser(browser, web, card, passed)', source)
        self.assertNotIn('storage_state(', source)

    def test_staged_font_policy_is_narrow_and_preserves_live_configuration(self):
        original = b"route-secret-placeholder\nContent-Security-Policy \"default-src 'self'; frame-ancestors 'none'\"\n"
        updated = release_candidate.portal_caddy_config(original)
        self.assertEqual(updated, original.replace(b"frame-ancestors 'none'", b"frame-ancestors 'none'; font-src 'self' data:"))
        self.assertEqual(release_candidate.portal_caddy_config(updated), updated)
        for invalid in (b'', original + original, original.replace(b"default-src 'self'", b"font-src 'none'")):
            with self.assertRaises(RuntimeError):
                release_candidate.portal_caddy_config(invalid)

    def test_readiness_distinguishes_new_login_from_legacy_rollback(self):
        page = Mock(status_code=200, headers={'Content-Type': 'text/html'})
        api = Mock(status_code=401, headers={})
        self.assertTrue(release_candidate.admin_readiness(page, api))
        self.assertFalse(release_candidate.admin_readiness(page, api, False))
        page.status_code = 401
        page.headers = {'WWW-Authenticate': 'Basic'}
        self.assertFalse(release_candidate.admin_readiness(page, api))
        self.assertTrue(release_candidate.admin_readiness(page, api, False))
        api.status_code = 200
        self.assertFalse(release_candidate.admin_readiness(page, api, False))

    def test_cookie_login_and_csrf_no_bootstrap_key(self):
        files = {
            '/etc/kiro-byok/gateway.env': b'ADMIN_KEY=never-send-this\n',
            '/etc/kiro-byok/admin-access.json': json.dumps({'username': 'admin', 'password': 'test-password'}).encode(),
        }
        ssh = Mock()
        sftp = Mock()
        sftp.open.side_effect = lambda name: io.BytesIO(files[name])
        ssh.open_sftp.return_value.__enter__ = Mock(return_value=sftp)
        ssh.open_sftp.return_value.__exit__ = Mock(return_value=False)
        http = Mock()
        http.get.return_value.json.return_value = {'csrfToken': 'csrf-example'}
        http.request.return_value.status_code = 200
        with patch.object(domain_acceptance.requests, 'Session', return_value=http):
            _, _, admin, request = domain_acceptance.connection(ssh)
            login = http.post.call_args.kwargs
            self.assertEqual(login['json'], {'username': 'admin', 'password': 'test-password'})
            self.assertEqual(login['headers'], {'Origin': domain_acceptance.BASE})
            self.assertNotIn('Authorization', admin)
            self.assertEqual(admin['x-csrf-token'], 'csrf-example')
            request('/api/v1/admin/cards/reveal', {'cardId': 'test'})
            self.assertEqual(http.request.call_args.kwargs['headers'], admin)
            request('/getUsageLimits', headers={'Authorization': 'Bearer user-test'})
            self.assertEqual(http.request.call_args.kwargs['headers'], {'Authorization': 'Bearer user-test'})
            self.assertFalse(http.request.call_args.kwargs['allow_redirects'])


if __name__ == '__main__':
    unittest.main()
