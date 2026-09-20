import launch_to_user_desktop
import time
import os

chrome = r"C:\Program Files\Google\Chrome\Application\chrome.exe"
url = "http://127.0.0.1:44044/?screen=screen-01"
user_data = os.path.join(os.environ.get('USERPROFILE', os.path.expanduser('~')), '.kiro-chrome-data')
log_path = os.path.abspath(r"apps\desktop-ui\launch.log")

cmd = f'"{chrome}" --enable-logging=file --log-file="{log_path}" --v=1 --disable-gpu --user-data-dir="{user_data}" --app={url} --window-size=500,680'
pid = launch_to_user_desktop.launch_on_default_desktop(cmd)
print('Launched cmd PID:', pid)
time.sleep(2)
if os.path.exists(log_path):
    with open(log_path, 'r', encoding='utf-8', errors='ignore') as f:
        print('LOG CONTENT:', f.read())
else:
    print('No log file produced!')
