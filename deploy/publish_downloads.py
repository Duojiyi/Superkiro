"""Historical Python packaging preparation only; remote publication is disabled."""
import hashlib, json, shutil, zipfile
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
VERSION='0.1.0-test.20260919+6d1902e'
def prepare():
    source=ROOT/'.acceptance/packages-6d1902e'; out=ROOT/'.acceptance/public-downloads';out.mkdir(exist_ok=True)
    releases=[]
    for platform,arch,label,requirements in [('windows','x64','Windows-X64','Windows 10/11 x64 · Microsoft Edge WebView2'),('macos','arm64','macOS-ARM64','macOS 13 或更新版本 · Apple Silicon · 测试版'),('macos','x64','macOS-X64','macOS 13 或更新版本 · Intel · 测试版')]:
        directory=source/f'Superkiro-{label}-unsigned';name=f'Superkiro-20260919-6d1902e-{platform}-{arch}.zip';target=out/name
        if platform=='windows':
            with zipfile.ZipFile(target,'w',compression=zipfile.ZIP_DEFLATED,compresslevel=6) as z:
                for p in sorted((directory/'Superkiro').rglob('*')):
                    if p.is_file(): z.write(p,p.relative_to(directory).as_posix())
        else: shutil.copyfile(directory/f'Superkiro-{arch.upper()}.zip',target)
        with zipfile.ZipFile(target) as z:
            names=z.namelist(); assert not any('server-ca.pem' in n or n.endswith(('.env','.key')) for n in names)
            assert any(n.endswith('patch-cli.exe' if platform=='windows' else '/patch-cli') for n in names)
            assert any(n.endswith('/apps/desktop-ui/desktop.js') for n in names)
            if platform=='macos': assert any(n.endswith('/Contents/MacOS/Superkiro') and (z.getinfo(n).external_attr>>16)&0o111 for n in names)
        releases.append(dict(platform=platform,arch=arch,version=VERSION,url='/downloads/'+name,sha256=hashlib.sha256(target.read_bytes()).hexdigest(),size=target.stat().st_size,systemRequirements=requirements,signature='unsigned',sourceCommit='6d1902e',buildRun=35412805646))
    (out/'releases.json').write_text(json.dumps({'releases':releases},indent=2,ensure_ascii=False),encoding='utf-8')
    print(json.dumps(releases,indent=2,ensure_ascii=False));return out,releases

def publish(ssh):
    raise RuntimeError(
        'Legacy Python desktop publication is retired. Use the native publisher '
        'with an explicit accepted artifact digest; macOS requires its own acceptance.'
    )

if __name__=='__main__':prepare()
