"""Announcement modal layout and interaction against the built desktop frontend."""
import functools, http.server, threading, json
from pathlib import Path
from playwright.sync_api import sync_playwright
from verify_fit import fixture
ROOT=Path(__file__).resolve().parent
OUT=ROOT/'verification'/'announcements'
OUT.mkdir(parents=True,exist_ok=True)
class Quiet(http.server.SimpleHTTPRequestHandler):
 def log_message(self,*args):pass
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),functools.partial(Quiet,directory=str(ROOT/'dist')))
threading.Thread(target=server.serve_forever,daemon=True).start()
try:
 with sync_playwright() as p:
  browser=p.chromium.launch()
  for width,height in [(480,540),(620,820)]:
   page=browser.new_page(viewport={'width':width,'height':height})
   errors=[]
   page.on('pageerror',lambda e:errors.append(str(e)))
   host=fixture()+r"""
const original=window.__TAURI_INTERNALS__.invoke;
window.__TAURI_INTERNALS__.invoke=async(command,args)=>args.path==='/api/announcements'?{announcements:[{id:'layout-test',title:'服务更新',content:'新的连接与诊断体验已经上线。公告内容会自动分页，不遮挡操作。\n'.repeat(80),level:'info',created_at:1790000000,expires_at:null}]}:original(command,args);
"""
   page.add_init_script(host)
   page.goto(f'http://127.0.0.1:{server.server_port}')
   page.get_by_role('button',name='公告').click()
   page.locator('.announcements-text:not(.announcements-measure)').filter(has_text='服务更新').wait_for()
   seen=[]
   for _ in range(100):
    layout=page.locator('dialog[open]').evaluate("""el=>{const b=el.getBoundingClientRect();const v=el.querySelector('.announcements-viewport'),t=v.querySelector('.announcements-text');return {outer:el.scrollHeight<=el.clientHeight&&el.scrollWidth<=el.clientWidth,inBounds:b.top>=0&&b.bottom<=innerHeight&&b.left>=0&&b.right<=innerWidth,textFits:t.scrollHeight<=v.clientHeight&&t.scrollWidth<=v.clientWidth,text:t.textContent}}""")
    assert layout['outer'] and layout['inBounds'] and layout['textFits'],layout
    seen.append(layout['text'])
    button=page.get_by_role('button',name='下一页',exact=True)
    if button.is_disabled():break
    button.click()
   assert ''.join(seen)=='服务更新\n\n'+'新的连接与诊断体验已经上线。公告内容会自动分页，不遮挡操作。\n'*80
   page.screenshot(path=str(OUT/f'{width}-{height}.png'))
   page.get_by_role('button',name='关闭',exact=True).click()
   assert not page.locator('dialog[open]').count()
   page.get_by_role('button',name='公告',exact=True).click()
   page.keyboard.press('Escape')
   assert not page.locator('dialog[open]').count()
   assert not errors,errors
   print('PASS announcement pagination, full text, viewport, close, reopen, Escape',width,height,len(seen))
   page.close()
  browser.close()
finally:
 server.shutdown();server.server_close()

