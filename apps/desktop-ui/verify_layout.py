"""Built UI layout regression. Real bridge preview + host-contract fixtures, not native E2E."""
import functools
import http.server
import json
import threading
from pathlib import Path
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parent
OUT = ROOT / "verification" / "layout"
OUT.mkdir(parents=True, exist_ok=True)
class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args): pass

# Matches desktop-host /api/status fields. No card, token, or real machine mutation.
HOST = """
window.isTauri = true;
window.scenario = SCENARIO;
window.attempted = false;
window.__TAURI_INTERNALS__ = {invoke: async (command, args) => {
  if (command === 'native') return args.method === 'get_remembered_card' ? null : args.method === 'get_close_behavior' ? 'tray' : true;
  if (args.path === '/api/operation') return {id:0,state:'idle'};
  if (args.path === '/api/status') {
    if (scenario === 'notice-error') throw 'network';
    const recovery = scenario === 'recovery' || attempted && scenario === 'activate-recovery';
    return {kiro_installed:true,kiro_install_path:'C:/Users/Example/AppData/Local/Programs/Kiro',kiro_version:'1.1.14',
      process_state:'Running',has_snapshot:recovery,recovery_pending:recovery,authenticated:false,
      model_service_available:null,tray_available:true,platform:'win32',app_version:'0.1.0',
      memory_maintenance:{enabled:true,mode:'automatic',threshold_mb:2500,cooldown_seconds:300,last_sample_mb:null,last_trim:null}};
  }
  if (args.path === '/api/verify-card') {
    if (scenario === 'error' || scenario === 'notice-error') throw 'certificate';
    return {success:true,authorization:{remainingPoints:100,totalPoints:100},gateway_url:'https://example.com'};
  }
  if (args.path === '/api/activate') { window.attempted = true; throw '[connection:apply] write permission https://private.example/token'; }
  return {success:true};
}};
"""
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(Quiet, directory=str(ROOT / 'dist')))
threading.Thread(target=server.serve_forever, daemon=True).start()
results = []
try:
    with sync_playwright() as p:
        browser = p.chromium.launch()
        for width, height in [(480,620),(384,496),(320,413),(240,310),(620,820)]:
            for scenario in ['normal','error','notice-error','preview','recovery','activate-recovery']:
                page = browser.new_page(viewport={'width':width,'height':height}, reduced_motion='reduce')
                errors = []
                page.on('pageerror', lambda e: errors.append(str(e)))
                if scenario != 'preview':
                    page.add_init_script(HOST.replace('SCENARIO', json.dumps(scenario)))
                page.goto(f'http://127.0.0.1:{server.server_port}')
                if scenario in ['error','notice-error','activate-recovery']:
                    page.get_by_label('输入你的卡密').fill('mock-only-card')
                    page.get_by_role('button',name='登录 →',exact=True).click()
                    if scenario == 'activate-recovery':
                        page.get_by_role('button',name='启用连接',exact=True).click()
                        page.get_by_role('dialog').get_by_role('button',name='启用连接',exact=True).click()
                        page.get_by_role('heading',name='本机配置待恢复').wait_for()
                        assert page.get_by_role('button',name='修复并重新连接').count() == 0
                        assert page.get_by_role('button',name='启用连接',exact=True).count() == 0
                        assert '[connection:apply]' in page.locator('.notice').inner_text()
                        assert 'private.example' not in page.locator('.notice').inner_text()
                    else:
                        page.get_by_role('button',name='重新登录 →').wait_for()
                elif scenario == 'recovery':
                    page.get_by_role('heading',name='本机配置待恢复').wait_for()
                elif scenario == 'preview':
                    page.get_by_role('button',name='关闭提示').wait_for()
                    assert any(text in page.locator('.notice').inner_text() for text in ['浏览器预览','本地服务已断开'])
                else:
                    page.get_by_role('heading',name='卡密登录').wait_for()
                page.wait_for_timeout(100)
                geometry = page.evaluate("""() => {
                    const region=document.querySelector('.content-scroll'), shell=document.querySelector('.shell');
                    return {documentHeight:document.documentElement.scrollHeight,documentWidth:document.documentElement.scrollWidth,
                      shellHeight:shell.getBoundingClientRect().height,scrollHeight:region.scrollHeight,clientHeight:region.clientHeight,
                      scrollWidth:region.scrollWidth,clientWidth:region.clientWidth,rootOverflow:getComputedStyle(document.body).overflow,
                      radius:getComputedStyle(shell).borderRadius};
                }""")
                assert geometry['documentHeight'] <= height and geometry['documentWidth'] <= width, (scenario,width,geometry)
                assert geometry['shellHeight'] == height, (scenario,width,geometry)
                assert geometry['scrollWidth'] <= geometry['clientWidth'], (scenario,width,geometry)
                assert geometry['rootOverflow'] == 'hidden'
                names = ['还原待恢复配置'] if 'recovery' in scenario else ['登录帮助 ↗','重新登录 →' if scenario in ['error','notice-error'] else '登录 →']
                if page.get_by_role('button',name='关闭提示').count(): names += ['关闭提示']
                for name in names:
                    button = page.get_by_role('button',name=name,exact=True)
                    button.scroll_into_view_if_needed()
                    box=button.bounding_box(); region=page.locator('.content-scroll').bounding_box()
                    assert box and region and box['y'] >= region['y']-1 and box['y']+box['height'] <= region['y']+region['height']+1, (scenario,width,name,box,region)
                if width == 320 and scenario == 'notice-error':
                    page.get_by_role('button',name='重新登录 →',exact=True).scroll_into_view_if_needed()
                    page.screenshot(path=str(OUT/'320-notice-error-bottom.png'),omit_background=True,animations='disabled')
                page.locator('.content-scroll').evaluate('(el)=>el.scrollTop=0')
                if width in [480,320]: page.screenshot(path=str(OUT/f'{width}-{scenario}.png'),omit_background=True,animations='disabled')
                if 'recovery' not in scenario:
                    assert page.get_by_role('button',name='无需卡密，查看诊断与恢复').count() == 0
                    assert page.get_by_role('button',name='打开官网').is_visible()
                assert not errors, errors
                results.append({'viewport':[width,height],'scenario':scenario,**geometry})
                print(f'PASS {width}x{height} {scenario}',flush=True)
                page.close()
        browser.close()
    (OUT/'results.json').write_text(json.dumps({'scope':'Host-contract fixtures and real no-host bridge preview; reduced effective viewports simulate zoom space, not native DPI acceptance','cases':results},ensure_ascii=False,indent=2),encoding='utf-8')
    print(f'PASS: {len(results)} layout/state combinations; viewport shell, internal-only scrolling, reachable controls, no horizontal overflow or runtime errors')
finally:
    server.shutdown()
    server.server_close()
