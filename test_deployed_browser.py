"""Headless browser acceptance using a pinned server-certificate SPKI, no global trust changes."""
import json
import sys
from pathlib import Path
from playwright.sync_api import sync_playwright
from test_deployed_server import connect, ROOT

config = json.load(sys.stdin)
ssh = connect(config['password'], use_proxy=config.get('use_proxy', True))
sftp = ssh.open_sftp()
credentials = json.loads(sftp.open('/etc/kiro-byok/admin-access.json').read())
sftp.close()
_, out, _ = ssh.exec_command('openssl s_client -connect 127.0.0.1:443 -servername 160.202.47.98 -CAfile /etc/kiro-byok/root.crt -verify_return_error </dev/null 2>/dev/null | openssl x509 -pubkey -noout | openssl pkey -pubin -outform DER | openssl dgst -sha256 -binary | openssl base64 -A')
spki = out.read().decode().strip()
assert len(spki) == 44, 'Invalid server public-key pin'
ssh.close()
errors = []
failed = []
with sync_playwright() as p:
    browser = p.chromium.launch(headless=True, proxy={'server': 'http://127.0.0.1:7897'},
        args=['--ignore-certificate-errors-spki-list=' + spki])
    context = browser.new_context(http_credentials={'username': 'admin', 'password': credentials['password']}, viewport={'width': 1440, 'height': 1000})
    page = context.new_page()
    page.on('pageerror', lambda e: errors.append(str(e)))
    page.on('response', lambda r: failed.append({'status': r.status, 'url': r.url}) if r.status >= 500 else None)
    page.goto('https://160.202.47.98/admin/', wait_until='networkidle')
    if not page.get_by_placeholder('输入管理员密钥 (x-admin-key)...').is_visible():
        page.get_by_role('button', name='未认证 (点击配置 Key)').click()
    page.get_by_placeholder('输入管理员密钥 (x-admin-key)...').fill(credentials['adminKey'])
    page.get_by_role('button', name='保存并验证').click()
    page.get_by_placeholder('输入管理员密钥 (x-admin-key)...').wait_for(state='hidden')
    tabs = ['概览与毛利看板', '卡密资产管理', '租户分组与虚拟化', '供应商与 Key 治理', '模型映射与定价工作台', '实时调用链追踪', '财务对账与报表', '服务公告下发', '安全、2FA 与审计']
    for tab in tabs:
        page.get_by_role('button', name=tab).click()
        page.wait_for_timeout(300)
        if tab == '供应商与 Key 治理':
            page.get_by_role('heading', name='添加或更新 Key / 获取候选模型').wait_for()
            assert page.get_by_label('API Key（新增必填，更新可留空）').get_attribute('type') == 'password'
            assert page.get_by_label('允许模型（每行一个精确 ID）').is_visible()
            assert page.get_by_role('button', name='获取模型草稿').is_visible()
            assert page.get_by_role('button', name='确认保存权限').is_visible()
        if tab == '租户分组与虚拟化':
            page.get_by_label('配置 JSON', exact=True).wait_for()
            page.wait_for_function("() => document.querySelector('textarea[aria-label=\"配置 JSON\"]').value.includes('groups')")
            assert 'groups' in json.loads(page.get_by_label('配置 JSON', exact=True).input_value())
        if tab == '模型映射与定价工作台':
            page.get_by_label('配置 JSON', exact=True).wait_for()
            page.wait_for_function("() => document.querySelector('textarea[aria-label=\"配置 JSON\"]').value.includes('rate_cards')")
            draft = json.loads(page.get_by_label('配置 JSON', exact=True).input_value())
            assert {'models', 'rate_cards', 'versions'} <= draft.keys()
            assert page.get_by_role('button', name='确认并发布').is_disabled()
        print('PASS browser tab: ' + tab, flush=True)
    page.get_by_role('button', name=tabs[0]).click()
    page.screenshot(path=str(ROOT / 'deployment-admin.png'), full_page=True)
    page.goto('https://160.202.47.98/portal', wait_until='networkidle')
    assert page.locator('input').count() > 0, 'Portal input missing'
    page.screenshot(path=str(ROOT / 'deployment-portal.png'), full_page=True)
    assert not errors, errors
    assert not failed, failed
    result = {'tabs': tabs, 'pageErrors': errors, 'serverErrors': failed, 'portal': 'rendered', 'tls': 'pinned server certificate SPKI'}
    (ROOT / 'deployment-browser-results.json').write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding='utf-8')
    browser.close()
print('BROWSER ACCEPTANCE PASSED', flush=True)
