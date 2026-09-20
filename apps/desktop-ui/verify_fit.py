"""Native-size browser fit audit using existing host fixtures; no real host calls."""
import ast
import functools
import http.server
import json
import itertools
import threading
from pathlib import Path
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parent
OUT = ROOT / 'verification' / 'fit'

def fixture():
    # Extract the existing literal without importing a script that runs its suite.
    tree = ast.parse((ROOT / 'verify_browser.py').read_text(encoding='utf-8'))
    return next(ast.literal_eval(n.args[0]) for n in ast.walk(tree)
                if isinstance(n, ast.Call) and isinstance(n.func, ast.Attribute)
                and n.func.attr == 'add_init_script')

def main():
    OUT.mkdir(parents=True, exist_ok=True)
    class Quiet(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *args): pass
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(Quiet, directory=str(ROOT / 'dist')))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    results, failures = [], []
    try:
        with sync_playwright() as p:
            browser = p.chromium.launch()
            cases=list(itertools.product([(480, 540), (620, 820)], ['login', 'pending', 'connected', 'usage-data', 'full-data', 'recovery']))
            cases.sort(key=lambda case: 0 if case==((480,540),'login') else 1 if case==((620,820),'connected') else 2)
            for (width, height), state in cases:
                page = browser.new_page(viewport={'width':width,'height':height})
                errors = []
                page.on('pageerror', lambda error: errors.append(str(error)))
                host = fixture()
                if state in ['connected', 'usage-data', 'full-data', 'recovery']:
                    host = host.replace("state==='connected'", "['connected','usage-data'].includes(state)")
                    host += '\nwindow.mockState=' + json.dumps('usage-data' if state=='full-data' else state) + ';'
                # Keep existing fixture behavior; populate host fields used by layout.
                host += '''
const fitInvoke = window.__TAURI_INTERNALS__.invoke;
window.__TAURI_INTERNALS__.invoke = async (command,args) => {
  if (command !== 'native' && args.path === '/api/memory/sample')
    return {total_memory_mb:1536,total_process_count:3,ide_memory_mb:1024,agent_memory_mb:512};
  if (command !== 'native' && args.path.startsWith('/api/doctor'))
    return {items:['Kiro Installation','Gateway Connectivity','Settings Configuration','Extension Patch','Authentication Token','网关 TLS 代理路径','真实 IDE 交互验收'].map((name,i)=>({name,level:i<2?'pass':'warning'}))};
  const value = await fitInvoke(command,args);
  if (command !== 'native' && args.path === '/api/status') Object.assign(value, {
    kiro_install_path:'C:/Users/Example/AppData/Local/Programs/Kiro',kiro_version:'1.1.14',
    tray_available:true,app_version:'0.1.0',
    authorization:{virtualPlanName:'PRO',remainingPoints:180,totalPoints:200},
    memory_maintenance:{enabled:true,mode:'automatic',threshold_mb:2500,cooldown_seconds:300,last_sample_mb:null,last_trim:null}
  });
  if (command !== 'native' && args.path === '/api/usage' && value.settledUsage)
    value.settledUsage.timezone='UTC';
  return value;
};
'''
                if state == 'full-data':
                    host += """
const fullInvoke=window.__TAURI_INTERNALS__.invoke;
window.__TAURI_INTERNALS__.invoke=async(command,args)=>{
 const v=await fullInvoke(command,args);
 if(args.path==='/api/status')v.authorization.validUntil=1792425600;
 if(args.path==='/api/usage'&&v.settledUsage){
  v.settledUsage.models=Array.from({length:13},(_,i)=>({name:'Model '+i+' long production model name',tokens:53220,points:.536}));
  v.settledUsage.daily=Array.from({length:7},(_,i)=>({date:'2026-09-'+(13+i),tokens:53220,points:.536}));
 }
 return v;
};
"""
                page.add_init_script(host)
                page.goto(f'http://127.0.0.1:{server.server_port}')
                if state in ['login', 'pending']:
                    page.get_by_role('heading', name='卡密登录').wait_for()
                if state == 'pending':
                    page.get_by_label('输入你的卡密').fill('mock-only-card')
                    page.get_by_role('button',name='登录 →',exact=True).click()
                if state not in ['login','recovery']:
                    page.get_by_role('navigation').wait_for()
                    assert page.get_by_role('navigation').get_by_role('button').count() == 2
                    for name in ['概览','设置']:
                        assert page.get_by_role('navigation').get_by_role('button',name=name,exact=True).is_visible()
                for view in (['login'] if state=='login' else ['doctor','doctor-checked','report'] if state=='recovery' else ['overview','overview-refresh','settings','settings-memory','settings-account']):
                    if view.startswith('settings-'):
                        if height > 600: continue
                        page.get_by_label('设置分组').select_option(view.split('-')[1])
                    elif view == 'doctor':
                        page.get_by_role('heading',name='本机配置待恢复').wait_for()
                    elif view == 'overview-refresh':
                        page.get_by_role('button',name='刷新 ↻',exact=True).click()
                    elif view == 'doctor-checked':
                        page.get_by_role('button',name='重新检测 ↻',exact=True).click()
                        page.get_by_text('网关 TLS 代理路径',exact=True).wait_for()
                    elif view == 'report':
                        page.get_by_role('button',name='查看诊断报告',exact=True).click()
                    elif view != 'login':
                        page.get_by_role('navigation').get_by_role('button',name={'overview':'概览','settings':'设置'}[view],exact=True).click()
                    if view.startswith('overview'):
                        assert page.get_by_text('今日已用积分',exact=True).is_visible()
                        assert 'Tokens' not in page.locator('.content-scroll').inner_text()
                    page.wait_for_timeout(500)
                    if state=='full-data':
                        page.evaluate("""() => {const root=document.querySelector('.content-scroll');root.querySelector('.fit-notice')?.remove();const n=document.createElement('aside');n.className='notice fit-notice';n.innerHTML='<span>本地服务暂未响应，正在重试检测。连接配置已应用，请在 Kiro 中验证真实模型对话。</span><button>×</button>';root.prepend(n);}""")
                    geometry = page.evaluate('''() => {
                        const r=document.querySelector('.content-scroll'), b=r.getBoundingClientRect();
                        const elements=[...r.querySelectorAll('button,input,h1,h2,p,.path-box,.row')].filter(e=>e.getClientRects().length);
                        const clipped=elements.filter(e=>{const v=e.getBoundingClientRect();return v.top<b.top-1||v.bottom>b.bottom+1||v.left<b.left-1||v.right>b.right+1}).map(e=>e.textContent||e.getAttribute('aria-label'));
                        const last=Math.max(...elements.map(e=>e.getBoundingClientRect().bottom));
                        return {clientHeight:r.clientHeight,scrollHeight:r.scrollHeight,clientWidth:r.clientWidth,scrollWidth:r.scrollWidth,scrollTop:r.scrollTop,clipped,bottomBlank:innerHeight-last};
                    }''')
                    name=f'{width}x{height}-{state}-{view}'
                    if view == 'settings' and height > 600:
                        buttons = page.locator('.settings-actions button')
                        assert buttons.count() == 6
                        for button in buttons.all():
                            bounds = button.bounding_box()
                            assert bounds and 30 <= bounds['height'] <= 36, (name, bounds)
                        assert page.locator('.settings-actions > p').count() == 0

                    if geometry['scrollHeight']>geometry['clientHeight'] or geometry['scrollWidth']>geometry['clientWidth'] or geometry['clipped'] or geometry['scrollTop']:
                        failures.append(name + ': overflow or clipped content')
                    if view=='login' and height==540 and geometry['bottomBlank']>80: failures.append(name+': excessive bottom blank')
                    if page.locator('.wave i').count():
                        bar=page.locator('.wave i').nth(16)
                        before=bar.evaluate('(e)=>getComputedStyle(e).transform')
                        page.wait_for_timeout(220)
                        after=bar.evaluate('(e)=>getComputedStyle(e).transform')
                        geometry['wave']={'before':before,'after':after}
                        if before==after or before=='none' or after=='none': failures.append(name+': no actual wave transform motion')
                        page.emulate_media(reduced_motion='reduce')
                        a=bar.evaluate('(e)=>({transform:getComputedStyle(e).transform,animations:e.getAnimations().length})')
                        page.wait_for_timeout(220)
                        b=bar.evaluate('(e)=>({transform:getComputedStyle(e).transform,animations:e.getAnimations().length})')
                        geometry['reducedMotion']=b
                        if a!=b or b['animations'] or b['transform']!='none': failures.append(name+': reduced-motion failed')
                    page.screenshot(path=str(OUT/f'{name}.png'),omit_background=True,animations='disabled')
                    page.emulate_media(reduced_motion='no-preference')
                    results.append({'case':name,**geometry,'errors':list(errors)})
                    print(name, json.dumps(geometry,ensure_ascii=False),flush=True)
                if errors: failures.extend(errors)
                page.close()
            browser.close()
        (OUT/'results.json').write_text(json.dumps({'scope':'Built dist with existing verify_browser.py host fixtures; CSS pixels, not native DPI acceptance','cases':results,'failures':failures},ensure_ascii=False,indent=2),encoding='utf-8')
        assert not failures, failures
        print(f'PASS {len(results)} native-size fit cases')
    finally:
        server.shutdown()
        server.server_close()

if __name__=='__main__': main()

