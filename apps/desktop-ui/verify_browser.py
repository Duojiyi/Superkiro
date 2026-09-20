"""Browser-only mocked UI verification, never a native or pixel acceptance test."""
import functools
import http.server
import json
from pathlib import Path
import threading
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parent
OUT = ROOT / 'verification'
OUT.mkdir(exist_ok=True)
class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args): pass
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(Quiet, directory=str(ROOT / 'dist')))
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
results = []
try:
    with sync_playwright() as p:
        browser = p.chromium.launch()
        page = browser.new_page(viewport={'width': 620, 'height': 820}, device_scale_factor=1)
        errors=[]
        page.on('pageerror', lambda e: errors.append(str(e)))
        page.add_init_script('''
window.isTauri = true; window.__TAURI_INTERNALS__ = {invoke: async (command, args) => {
  const state = window.mockState || new URLSearchParams(location.search).get('state') || 'login';
  if(command === 'native') return args.method === 'get_remembered_card' ? null : args.method === 'get_close_behavior' ? 'tray' : true;
  if (args.path === '/api/operation') return {id:0,state:'idle'};
  if(args.path === '/api/status') return {kiro_installed:state!=='missing',process_state:state==='connected'?'Running':'NotRunning',platform:'win32',recovery_pending:state==='recovery',authenticated:['connected','usage-data','usage-error'].includes(state),has_snapshot:['connected','usage-data','usage-error'].includes(state),model_service_available:null,memory_maintenance:window.mockMaintenance};
  if(args.path === '/api/verify-card') {if(state==='login-error')throw 'secret upstream error';return {success:true,authorization:{virtualPlanName:'PRO',remainingPoints:180,totalPoints:200,isExpired:state==='expired'},gateway_url:'https://example.com'};}
  if(args.path === '/api/usage') {if(state==='usage-error')throw 'network';return {usage:{usageBreakdownList:[{dimensionType:'CREDIT',currentUsageWithPrecision:state==='usage-data'?20:0,usageLimitWithPrecision:200}]},settledUsage:state==='usage-data'?{todayPoints:20,todayTokens:1000,timezone:'UTC',totalTokens:1000,daily:[{date:'2026-09-19',points:20,tokens:1000}],models:[{name:'Mock model',tokens:1000,points:20}]}:null};}
  if(args.path.startsWith('/api/doctor'))return {items:[{name:'Kiro Installation',level:'pass'},{name:'Gateway Connectivity',level:'warning'}]};
  if(args.path === '/api/activate') return new Promise(()=>{});
  if(args.path === '/api/restore') throw 'restore failed';
  return {success:true};
}};
''')
        def shot(name):
            page.mouse.move(0, 0)
            # Await finite entrance animations, not perpetual waves/spinners.
            page.evaluate('''async () => {
              await document.fonts.ready;
              await Promise.all(document.getAnimations().filter(a => Number.isFinite(a.effect.getComputedTiming().endTime)).map(a => a.finished.catch(() => {})));
            }''')
            alert_checks = page.locator('.panel.alert').evaluate_all('''panels => {
              const luminance = rgb => rgb.match(/[0-9.]+/g).slice(0,3).map(Number).map(v => {v /= 255; return v <= .04045 ? v/12.92 : ((v+.055)/1.055)**2.4;}).reduce((n,v,i) => n+v*[.2126,.7152,.0722][i],0);
              return panels.flatMap(panel => [...panel.querySelectorAll('h2,p')].map(el => {
                const fg=getComputedStyle(el).color, bg=getComputedStyle(panel).backgroundColor;
                const a=luminance(fg),b=luminance(bg);let opacity=1;
                for(let node=el;node;node=node.parentElement)opacity*=Number(getComputedStyle(node).opacity);
                return {text:el.textContent,color:fg,background:bg,opacity,contrast:(Math.max(a,b)+.05)/(Math.min(a,b)+.05)};
              }));
            }''')
            assert all(item['opacity'] == 1 and item['contrast'] >= 4.5 for item in alert_checks), (name, alert_checks)
            assert page.evaluate('document.documentElement.scrollWidth <= innerWidth'), name+' horizontal overflow'
            assert page.evaluate("['html','body','#root'].every(s=>getComputedStyle(document.querySelector(s)).backgroundColor==='rgba(0, 0, 0, 0)')"), name+' opaque root'
            assert page.locator('.primary').evaluate_all("buttons=>buttons.every(b=>getComputedStyle(b).backgroundImage==='linear-gradient(rgb(187, 195, 186), rgb(238, 238, 229))')"), name+' inconsistent primary color'
            page.screenshot(path=str(OUT / (name+'.png')), full_page=True, omit_background=True, animations='disabled')
            results.append({'alertChecks':alert_checks,'state':name,'viewport':page.viewport_size,'scrollHeight':page.evaluate('document.documentElement.scrollHeight')})
        def start(state, logged=True):
            page.goto(f'http://127.0.0.1:{server.server_port}/?state={state}')
            page.evaluate('(s)=>window.mockState=s',state)
            if logged and state in ['connected','usage-data','usage-error']:
                page.get_by_role('navigation').wait_for()
            elif logged:
                page.get_by_label('输入你的卡密').fill('mock-only-card')
                page.get_by_role('button',name='登录 →',exact=True).click()
                page.get_by_role('navigation').wait_for()
                page.get_by_role('button',name='刷新 ↻',exact=True).click() if state=='missing' else None
        start('login',False);shot('01-login-620')
        page.set_viewport_size({'width':480,'height':620})
        assert page.evaluate('document.documentElement.scrollHeight <= innerHeight'), 'login 480x620 vertical overflow'
        assert page.get_by_role('button',name='打开官网').bounding_box()['y'] + page.get_by_role('button',name='打开官网').bounding_box()['height'] <= 620
        heights = page.locator('.login .wave i').evaluate_all('(bars)=>bars.map(b=>parseFloat(getComputedStyle(b).height))')
        assert len(heights) == 33 and heights.index(max(heights)) == 16
        assert all(heights[i] < heights[i+1] for i in range(16))
        assert all(abs(heights[i]-heights[-1-i]) < 0.1 for i in range(16))
        assert page.locator('.login .primary').evaluate('(b)=>getComputedStyle(b).backgroundImage') == 'linear-gradient(rgb(187, 195, 186), rgb(238, 238, 229))'
        shot('01-login-480')
        start('login-error',False);page.get_by_label('输入你的卡密').fill('mock-only-card');page.get_by_role('button',name='登录 →',exact=True).click();page.get_by_role('button',name='重新登录 →').wait_for()
        assert page.evaluate('document.documentElement.scrollHeight <= innerHeight'), 'login error 480x620 vertical overflow'
        for name in ['重新登录 →','打开官网']:
            box=page.get_by_role('button',name=name,exact=True).bounding_box()
            assert box and box['y'] >= 0 and box['y'] + box['height'] <= 620, name+' clipped'
        shot('11-login-error')
        page.set_viewport_size({'width':620,'height':820})
        start('connected');page.get_by_role('heading',name='连接配置已应用').wait_for();shot('02-configured-unverified')
        start('usage-data');page.get_by_role('button',name='刷新 ↻',exact=True).click();page.get_by_text('20 积分',exact=True).wait_for();shot('04-usage-data')
        start('pending');shot('03-pending')
        page.get_by_role('button',name='启用连接',exact=True).click();shot('14-confirm');page.get_by_role('button',name='取消',exact=True).click()
        start('connected');page.get_by_role('button',name='刷新 ↻',exact=True).click();page.get_by_text('今日已用积分',exact=True).wait_for();shot('12-usage-empty')
        page.evaluate("window.mockState='usage-error'");page.get_by_role('button',name='刷新 ↻',exact=True).click();page.get_by_text('用量刷新失败，保留最近确认余额；今日用量暂不可用。').wait_for();shot('04-usage-error')
        start('recovery',False);page.get_by_role('heading',name='本机配置待恢复').wait_for();page.get_by_role('button',name='重新检测 ↻').click();page.get_by_role('button',name='查看诊断报告').wait_for(state='visible');shot('05-diagnostics')
        page.get_by_role('button',name='查看诊断报告').click();shot('13-report')
        start('pending');page.get_by_role('button',name='设置',exact=True).click();page.get_by_text('Kiro 未运行').wait_for();assert page.get_by_role('button',name='立即整理',exact=True).is_disabled();shot('06-settings-no-process')
        page.evaluate("window.mockMaintenance={enabled:true,mode:'automatic',threshold_mb:2500,cooldown_seconds:300,last_sample_mb:null,last_trim:null}");page.get_by_role('button',name='重新采样 ↻').click();page.get_by_text('自动维护已开启',exact=True).wait_for();assert page.get_by_role('button',name='立即整理',exact=True).is_disabled();shot('06-automatic-no-process')
        start('expired');page.get_by_role('heading',name='卡密已到期').wait_for();shot('09-expired')
        start('pending');page.evaluate("window.mockState='missing'");page.get_by_role('button',name='刷新 ↻',exact=True).click();page.get_by_role('heading',name='未找到 Kiro').wait_for();shot('07-missing')
        start('pending');page.get_by_role('button',name='启用连接',exact=True).click();page.get_by_role('dialog').get_by_role('button',name='启用连接',exact=True).click();page.get_by_role('heading',name='正在连接 Kiro').wait_for();shot('08-connecting')
        start('pending');page.get_by_role('button',name='设置',exact=True).click();page.get_by_role('button',name='切换卡密').click();page.get_by_role('button',name='确认并继续').click();page.get_by_role('heading',name='恢复未完成').wait_for();shot('10-restore-failed')
        assert not errors,errors
        browser.close()
    (OUT/'results.json').write_text(json.dumps({'scope':'browser mock only; no native E2E or pixel acceptance','states':results,'pageErrors':errors},ensure_ascii=False,indent=2),encoding='utf-8')
    print(json.dumps(results,ensure_ascii=False,indent=2))
finally:
    server.shutdown();server.server_close();thread.join()



