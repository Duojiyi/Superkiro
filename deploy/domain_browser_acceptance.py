"""Production browser acceptance, using a disposable card and read-only admin pages."""
import json,uuid
from urllib.parse import urlsplit
from pathlib import Path
from playwright.sync_api import sync_playwright,expect
from deploy.domain_acceptance import ROOT,BASE,connection

# Every check below is an assert; optimized Python would skip them all and still pass.
if not __debug__:
    raise SystemExit('Run without -O: these checks are assertions')

def run(ssh):
    _,s,_,req=connection(ssh);card=None;results=[]
    out=ROOT/'.acceptance/domain-browser';out.mkdir(exist_ok=True)
    def passed(name):results.append({'check':name,'passed':True});print('PASS browser '+name,flush=True)
    try:
        card=req('/api/v1/admin/cards/batch',{'count':1,'templateId':'tier-1000','groupId':'group-pro-plus','note':'browser acceptance disposable'}).json()['cards'][0]
        with sync_playwright() as p:
            browser=p.chromium.launch()
            context=browser.new_context(viewport={'width':1440,'height':1000});page=context.new_page();errors=[]
            page.on('pageerror',lambda e:errors.append(type(e).__name__))
            for width in [1440,375]:
                page.set_viewport_size({'width':width,'height':900})
                for route,label in [('/','home'),('/device','device'),('/docs','docs')]:
                    r=page.goto(BASE+route);assert r.status==200
                    expect(page.locator('h1:visible')).to_be_visible()
                    assert page.evaluate('document.documentElement.scrollWidth <= innerWidth+1')
                    if route=='/':
                        # A misplaced keyframes brace previously swallowed these rules.
                        expect(page.locator('.platform-grid')).to_have_css('display', 'grid')
                        expect(page.locator('.release code').first).to_have_css('display', 'block')
                        expect(page.locator('[data-download="windows-x64"]:visible').first).to_have_attribute('href', __import__('re').compile(r'.*?/downloads/Superkiro-.*'))
                    page.screenshot(path=str(out/f'{label}-{width}.png'),full_page=True)
                    passed(label+' '+str(width))
            page.goto(BASE+'/device');page.locator('#card').fill(card['rawCode']);page.locator('#verify').click()
            expect(page.locator('#account')).to_be_visible()
            state=req('/api/v1/portal/query',{'card':card['rawCode']}).json()
            assert state['activatedAt'] is None and not state['boundDevices'];passed('query is read-only')
            device='browser-'+uuid.uuid4().hex
            req('/oauth/token',{'card_key':card['rawCode'],'device_id':device})
            page.locator('#reset-card').click();page.locator('#card').fill(card['rawCode']);page.locator('#verify').click()
            expect(page.locator('#request-unbind')).to_be_enabled();page.locator('#request-unbind').click()
            expect(page.locator('#confirm-dialog')).to_be_visible();page.locator('#confirm-unbind').click()
            expect(page.locator('#recovery')).to_be_visible()
            assert not req('/api/v1/portal/query',{'card':card['rawCode']}).json()['boundDevices'];passed('confirmed device unbind reaches backend')
            assert page.evaluate('localStorage.length===0 && sessionStorage.length===0');assert not errors;context.close()
            with ssh.open_sftp() as f: web=json.loads(f.open('/etc/kiro-byok/admin-access.json').read())
            verify_admin_browser(browser, web, card, passed)
            browser.close()
    finally:
        try:
            if card:req('/api/v1/admin/cards/status',{'cardId':card['cardId'],'action':'ban','reason':'browser acceptance completed'})
        finally:(out/'results.json').write_text(json.dumps(results,indent=2,ensure_ascii=False),encoding='utf-8');s.close()


def verify_admin_browser(browser, credentials, card, passed):
    """Real browser cookie flow. No saved auth state, Basic auth, or raw admin key.

    Invoked only by the explicitly authorized acceptance run, which issues a
    disposable card. Never run this production entry point for offline tests.
    """
    ctx = browser.new_context(viewport={'width': 1440, 'height': 1000})
    try:
        page = ctx.new_page()
        errors = []
        page.on('pageerror', lambda error: errors.append(type(error).__name__))
        response = page.goto(BASE + '/admin/')
        assert response.status == 200
        assert 'www-authenticate' not in response.headers
        assert ctx.request.get(BASE + '/api/v1/admin/cards').status == 401
        # Initial anonymous session check opens the form automatically.
        expect(page.get_by_label('密码', exact=True)).to_be_visible()
        page.get_by_label('用户名', exact=True).fill(credentials.get('username', 'admin'))
        page.get_by_label('密码', exact=True).fill(credentials['password'])
        with page.expect_response(lambda r: r.url == BASE + '/api/v1/admin/session' and r.request.method == 'POST') as login:
            page.locator('form').get_by_role('button', name='登录', exact=True).click()
        assert login.value.status == 200
        expect(page.get_by_role('button', name='管理会话', exact=True)).to_be_visible()
        cookies = [c for c in ctx.cookies(BASE) if c['name'] == '__Host-admin_session']
        assert len(cookies) == 1
        cookie = cookies[0]
        assert cookie['secure'] and cookie['httpOnly'] and cookie['sameSite'] == 'Strict'
        assert cookie['path'] == '/' and cookie['domain'] == urlsplit(BASE).hostname
        assert page.evaluate("!document.cookie.includes('__Host-admin_session=')")
        session = page.evaluate("""async () => {
            const r = await fetch('/api/v1/admin/session', {credentials: 'same-origin'});
            return {status: r.status, body: await r.json()};
        }""")
        assert session['status'] == 200 and session['body']['csrfToken']
        csrf = session['body']['csrfToken']
        reveal = page.evaluate("""async ({cardId, csrf}) => {
            const r = await fetch('/api/v1/admin/cards/reveal', {
                method: 'POST', credentials: 'same-origin',
                headers: {'Content-Type': 'application/json', 'x-csrf-token': csrf},
                body: JSON.stringify({cardId})
            });
            return {status: r.status, cache: r.headers.get('Cache-Control'), body: await r.json()};
        }""", {'cardId': card['cardId'], 'csrf': csrf})
        assert reveal['status'] == 200 and reveal['cache'] == 'no-store'
        assert reveal['body']['success'] is True and reveal['body']['rawCode'] == card['rawCode']
        passed('admin form login, secure cookie, session CSRF and reveal')
        nav = page.get_by_role('navigation', name='管理导航')
        for label in ['运营概览', '卡密资产', '分组与权益', '供应商与 Key', '模型与定价', '调用追踪', '财务对账', '公告管理', '安全与审计']:
            nav.get_by_role('button', name=label, exact=True).click()
            expect(page.get_by_role('heading', name=label, exact=True)).to_be_visible()
            if label in ('分组与权益', '模型与定价'):
                expect(page.get_by_role('status')).to_contain_text('当前配置已加载')
                expect(page.get_by_role('button', name='重新读取配置', exact=True)).to_be_enabled()
            elif label == '财务对账':
                expect(page.get_by_role('button', name='重新读取财务配置', exact=True)).to_be_enabled()
            passed('admin ' + label)
        with page.expect_response(lambda r: r.url == BASE + '/api/v1/admin/session/revoke' and r.request.method == 'POST') as logout:
            page.get_by_role('button', name='退出', exact=True).click()
        assert logout.value.status == 200
        assert logout.value.request.headers.get('x-csrf-token') == csrf
        expect(page.get_by_label('密码', exact=True)).to_be_visible()
        assert not any(c['name'] == '__Host-admin_session' for c in ctx.cookies(BASE))
        assert ctx.request.get(BASE + '/api/v1/admin/session').status == 401
        assert ctx.request.get(BASE + '/api/v1/admin/cards').status == 401
        # Reinsert the actual pre-logout cookie, proving server revocation, not
        # merely that the browser deleted its cookie. Never serialize it.
        replay = browser.new_context()
        try:
            replay.add_cookies(cookies)
            assert replay.request.get(BASE + '/api/v1/admin/cards').status == 401
        finally:
            replay.close()
        assert page.evaluate('localStorage.length===0 && sessionStorage.length===0')
        assert not errors
        passed('admin logout clears cookie and revokes the old session')
    finally:
        ctx.close()
