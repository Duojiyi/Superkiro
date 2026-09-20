import ctypes
from ctypes import wintypes
import os
import sys

kernel32 = ctypes.windll.kernel32

class STARTUPINFO(ctypes.Structure):
    _fields_ = [
        ('cb', wintypes.DWORD),
        ('lpReserved', wintypes.LPWSTR),
        ('lpDesktop', wintypes.LPWSTR),
        ('lpTitle', wintypes.LPWSTR),
        ('dwX', wintypes.DWORD),
        ('dwY', wintypes.DWORD),
        ('dwXSize', wintypes.DWORD),
        ('dwYSize', wintypes.DWORD),
        ('dwXCountChars', wintypes.DWORD),
        ('dwYCountChars', wintypes.DWORD),
        ('dwFillAttribute', wintypes.DWORD),
        ('dwFlags', wintypes.DWORD),
        ('wShowWindow', wintypes.WORD),
        ('cbReserved2', wintypes.WORD),
        ('lpReserved2', ctypes.c_char_p),
        ('hStdInput', wintypes.HANDLE),
        ('hStdOutput', wintypes.HANDLE),
        ('hStdError', wintypes.HANDLE)
    ]

class PROCESS_INFORMATION(ctypes.Structure):
    _fields_ = [
        ('hProcess', wintypes.HANDLE),
        ('hThread', wintypes.HANDLE),
        ('dwProcessId', wintypes.DWORD),
        ('dwThreadId', wintypes.DWORD)
    ]

def launch_on_default_desktop(cmd: str):
    si = STARTUPINFO()
    si.cb = ctypes.sizeof(STARTUPINFO)
    si.lpDesktop = r"WinSta0\Default"

    pi = PROCESS_INFORMATION()

    # DETACHED_PROCESS = 0x00000008, CREATE_NEW_PROCESS_GROUP = 0x00000200
    flags = 0x00000008 | 0x00000200

    res = kernel32.CreateProcessW(
        None,
        cmd,
        None,
        None,
        False,
        flags,
        None,
        None,
        ctypes.byref(si),
        ctypes.byref(pi)
    )

    if not res:
        err = kernel32.GetLastError()
        print(f"[!] CreateProcess failed: {err}")
        return None

    print(f"[√] Successfully launched process on WinSta0\\Default! PID: {pi.dwProcessId}")
    kernel32.CloseHandle(pi.hThread)
    kernel32.CloseHandle(pi.hProcess)
    return pi.dwProcessId

if __name__ == '__main__':
    screen = sys.argv[1] if len(sys.argv) > 1 else "screen-01"
    chrome = r"C:\Program Files\Google\Chrome\Application\chrome.exe"
    import time
    ts = int(time.time())
    url = f"http://127.0.0.1:44044/?screen={screen}&_t={ts}"
    user_data = os.path.join(os.environ.get('USERPROFILE', os.path.expanduser('~')), '.kiro-chrome-data')
    cmd_line = f'"{chrome}" --disable-gpu --disable-software-rasterizer --user-data-dir="{user_data}" --app={url} --window-size=458,580'
    launch_on_default_desktop(cmd_line)
