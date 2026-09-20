"""Native WebView2 smoke test. Uses mocked CLI; never changes Kiro or cloud cards."""
import json, threading, time
from pathlib import Path
from http.server import ThreadingHTTPServer
from unittest.mock import patch
import webview
import run_desktop as bridge

def main():
    server=ThreadingHTTPServer(('127.0.0.1',0),bridge.SecureBridgeHandler)
    worker=threading.Thread(target=server.serve_forever,daemon=True);worker.start()
    controls=bridge.DesktopWindow()
    window=webview.create_window('KIRO native verification',f'http://127.0.0.1:{server.server_port}/#token={bridge.SESSION_TOKEN}',js_api=controls,width=441,height=541,frameless=True,hidden=False,resizable=False,min_size=(320,300))
    controls._window=window
    result={}
    def wait_for(expression):
        deadline=time.monotonic()+15
        while time.monotonic()<deadline:
            if window.evaluate_js(expression): return
            time.sleep(.1)
        raise AssertionError(expression)
    def audit():
        try:
            if not window.events.loaded.wait(20):raise AssertionError('WebView page did not load')
            wait_for("!!window.pywebview?.api?.screen")
            wait_for("document.getElementById('bridge-state').textContent === '本地服务已连接'")
            window.evaluate_js("window.nativeErrors=[]; addEventListener('unhandledrejection', e=>nativeErrors.push(String(e.reason)))")
            result['activation']=window.evaluate_js("({width:innerWidth,height:innerHeight,nav:document.querySelectorAll('nav,footer').length,title:document.title})")
            assert result['activation']['width']==441 and result['activation']['height']==541,result
            assert controls.screen('settings','wrong-token') is False
            assert controls.screen('not-a-screen',bridge.SESSION_TOKEN) is False
            window.evaluate_js("document.querySelector('#connect [data-view=settings]').click()")
            wait_for("innerWidth===690 && innerHeight===357 && !document.getElementById('settings').hidden")
            result['settings']={'width':690,'height':357}
            window.evaluate_js("document.getElementById('settings-back').click()")
            wait_for("innerWidth===441 && innerHeight===541 && !document.getElementById('connect').hidden")
            result['roundtrip']=True
            result['passed']=True
        except Exception as e:
            result['error']=str(e);result['passed']=False
            result['failure_state']=window.evaluate_js("({width:innerWidth,height:innerHeight,view:currentView,settingsHidden:document.getElementById('settings').hidden,api:Object.keys(window.pywebview.api),errors:window.nativeErrors,tokenLength:token.length})")
        finally:
            window.destroy()
    try:
        with patch.object(bridge,'run_patch_cli',return_value=(0,json.dumps({'kiro_installed':True,'kiro_version':'test','process_state':'Running','authenticated':False}))):
            webview.start(audit,gui='edgechromium',debug=False,private_mode=True)
    finally:
        server.shutdown();server.server_close();worker.join(5)
    Path('.acceptance/native-webview-result.json').write_text(json.dumps(result,ensure_ascii=False,indent=2),encoding='utf-8')
    print(json.dumps(result,ensure_ascii=False))
    if not result.get('passed'):raise SystemExit(1)

if __name__=='__main__':main()

