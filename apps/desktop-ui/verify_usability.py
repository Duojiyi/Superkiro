"""Independent edge-state checks. All native/API calls are in-page mocks."""
import functools
import http.server
import json
import threading
from pathlib import Path
from playwright.sync_api import sync_playwright
from verify_fit import fixture

ROOT = Path(__file__).resolve().parent
OUT = ROOT / 'verification' / 'usability'

def main():
    OUT.mkdir(parents=True, exist_ok=True)
    class Quiet(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *args): pass
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(Quiet, directory=str(ROOT / 'dist')))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    results = []
    try:
        with sync_playwright() as p:
            browser = p.chromium.launch()
            for width, height in [(480, 540), (620, 820)]:
                page = browser.new_page(viewport={'width': width, 'height': height}, reduced_motion='reduce')
                errors = []
                page.on('pageerror', lambda e: errors.append(str(e)))
                page.add_init_script(fixture() + '''
const guestInvoke=window.__TAURI_INTERNALS__.invoke;
window.__TAURI_INTERNALS__.invoke=async(command,args)=>{
 const value=await guestInvoke(command,args);
 if(args.path==='/api/status'&&new URLSearchParams(location.search).get('state')==='recovery')value.recovery_pending=true;
 return value;
};
''')
                def check(name, selector='.content-scroll'):
                    page.evaluate('document.fonts.ready')
                    box = page.locator(selector)
                    geometry = box.evaluate('''el => {
                        const b=el.getBoundingClientRect();
                        const nodes=[...el.querySelectorAll('button,input,select,h1,h2,p,pre')].filter(e=>e.getClientRects().length);
                        return {height:el.clientHeight,scrollHeight:el.scrollHeight,width:el.clientWidth,scrollWidth:el.scrollWidth,
                          clipped:nodes.filter(e=>{const r=e.getBoundingClientRect();const bounds=e.closest('.toast')?.getBoundingClientRect()||b;return r.top<bounds.top-1||r.bottom>bounds.bottom+1||r.left<bounds.left-1||r.right>bounds.right+1}).map(e=>e.textContent),
                          overlaysInBounds:[...el.querySelectorAll('.toast')].every(e=>{const r=e.getBoundingClientRect();return r.top>=0&&r.bottom<=innerHeight&&r.left>=0&&r.right<=innerWidth}),
                          inBounds:b.top>=0&&b.bottom<=innerHeight&&b.left>=0&&b.right<=innerWidth};
                    }''')
                    results.append({'case': f'{width}x{height}-{name}', **geometry})
                    assert geometry['height'] >= geometry['scrollHeight'] and geometry['width'] >= geometry['scrollWidth'] and not geometry['clipped'] and geometry['inBounds'] and geometry['overlaysInBounds'], results[-1]
                    page.screenshot(path=str(OUT / f'{width}x{height}-{name}.png'))
                def start(state, login=False):
                    page.goto(f'http://127.0.0.1:{server.server_port}/?state={state}')
                    if login:
                        page.get_by_label('输入你的卡密').fill('mock-only-card')
                        page.get_by_role('button',name='登录 →',exact=True).click()
                start('login-error', True)
                page.get_by_role('button',name='重新登录 →').wait_for()
                check('login-error')
                help_button=page.get_by_role('button',name='登录帮助 ↗')
                help_button.click()
                check('login-help','dialog[open]')
                assert page.get_by_role('button',name='知道了').evaluate('e=>e===document.activeElement')
                page.keyboard.press('Escape')
                assert help_button.evaluate('e=>e===document.activeElement')
                assert page.get_by_role('button',name='无需卡密，查看诊断与恢复').count() == 0
                start('recovery')
                page.get_by_role('heading',name='本机配置待恢复').wait_for()
                page.get_by_role('button',name='返回卡密登录').click()
                page.get_by_role('heading',name='卡密登录').wait_for()
                for state, heading in [('missing','未找到 Kiro'),('expired','卡密已到期')]:
                    start(state,True)
                    page.get_by_role('heading',name=heading).wait_for()
                    check(state)
                start('pending',True)
                page.get_by_role('button',name='启用连接',exact=True).click()
                check('activate-confirm','dialog[open]')
                assert page.get_by_role('button',name='取消').evaluate('e=>e===document.activeElement')
                page.locator('dialog[open]').get_by_role('button',name='启用连接',exact=True).click()
                page.get_by_role('heading',name='正在连接 Kiro').wait_for()
                check('connecting')
                start('connected')
                page.get_by_role('button',name='还原 Kiro 配置',exact=True).click()
                check('restore-confirm','dialog[open]')
                page.get_by_role('button',name='确认并继续').click()
                page.get_by_role('heading',name='恢复未完成').wait_for()
                check('restore-failed')
                start('usage-error')
                page.get_by_role('button',name='刷新 ↻',exact=True).click()
                page.get_by_text('用量刷新失败，保留最近确认余额；今日用量暂不可用。').wait_for()
                check('usage-error')
                start('connected')
                page.get_by_role('button',name='设置',exact=True).click()
                if height <= 600:
                    page.get_by_label('设置分组').select_option('account')
                page.get_by_role('button',name='解除设备绑定',exact=True).click()
                check('unbind-confirm','dialog[open]')
                page.keyboard.press('Escape')
                assert page.get_by_role('button',name='解除设备绑定',exact=True).evaluate('e=>e===document.activeElement')
                start('recovery')
                page.get_by_role('heading',name='本机配置待恢复').wait_for()
                for name in ['概览','用量','诊断','设置']:
                    assert page.get_by_role('button',name=name,exact=True).count()==0
                check('guest-recovery')
                page.get_by_role('button',name='重新检测 ↻').click()
                page.get_by_role('button',name='查看诊断报告').click()
                page.get_by_role('heading',name='诊断报告').wait_for()
                check('guest-report')
                start('pending',True)
                page.get_by_role('button',name='设置',exact=True).click()
                if height <= 600:
                    page.get_by_label('设置分组').select_option('account')
                page.evaluate('''() => {
                    const original=window.__TAURI_INTERNALS__.invoke;
                    window.failClear=true;
                    window.__TAURI_INTERNALS__.invoke=async(command,args)=>{
                        if(args.path==='/api/restore')return {success:true};
                        if(command==='native'&&args.method==='clear_remembered_card'){
                            if(window.failClear)throw 'mock storage failure';
                            return true;
                        }
                        return original(command,args);
                    };
                }''')
                page.get_by_role('button',name='切换卡密').click()
                page.get_by_role('button',name='确认并继续').click()
                page.get_by_role('heading',name='卡密登录').wait_for()
                page.get_by_text('配置已还原，但系统保存的卡密未能清除，请在登录页取消记住卡密后重试。').wait_for()
                check('credential-clear-failed')
                page.evaluate('window.failClear=false')
                page.get_by_label('记住卡密').uncheck()
                page.get_by_text('系统保存的卡密已清除。').wait_for()
                check('credential-clear-retried')
                assert not errors, errors
                page.close()
            browser.close()
        (OUT / 'results.json').write_text(json.dumps(results, ensure_ascii=False, indent=2), encoding='utf-8')
        print(f'PASS {len(results)} edge-state viewport cases, dialog descriptions, initial/return focus and login recovery route')
    finally:
        server.shutdown()
        server.server_close()

if __name__ == '__main__':
    main()
