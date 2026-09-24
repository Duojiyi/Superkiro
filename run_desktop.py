#!/usr/bin/env python3
"""
KIRO 智能极速接管中心 · 桌面独立窗口启动器与本地安全桥接服务 (Spec §9, T08)
"""
import os
import sys
import subprocess
import time
import secrets
import json
import urllib.parse
import urllib.request
import urllib.error
import ssl
import math
import threading
from http.server import ThreadingHTTPServer, BaseHTTPRequestHandler

BASE_DIR = getattr(sys, "_MEIPASS", os.path.dirname(os.path.abspath(__file__)))
DEFAULT_GATEWAY_URL = "https://kiro.rent"
# Public TLS by default; KIRO_GATEWAY_CA_CERT is an explicit development override.

UI_DIR = os.path.join(BASE_DIR, 'apps', 'desktop-ui')
SESSION_TOKEN = secrets.token_hex(16)
LAST_HEARTBEAT = time.time()
HEARTBEAT_LOCK = threading.Lock()
SERVER_SHUTDOWN = threading.Event()
BUILD_MODE = "release"

def desktop_preferences_path():
    if sys.platform == "win32":
        root = os.environ.get("LOCALAPPDATA", os.path.expanduser("~"))
    elif sys.platform == "darwin":
        root = os.path.expanduser("~/Library/Application Support")
    else:
        root = os.environ.get("XDG_CONFIG_HOME", os.path.expanduser("~/.config"))
    return os.path.join(root, "Superkiro", "preferences.json")


def desktop_environment():
    environment = dict(os.environ)
    try:
        with open(desktop_preferences_path(), encoding="utf-8") as file:
            preferences = json.load(file)
        path = preferences.get("install_path")
        if isinstance(path, str) and os.path.isabs(path):
            environment["SUPERKIRO_INSTALL_DIR"] = path
    except (OSError, ValueError, AttributeError):
        pass
    return environment


def run_patch_cli(args: list[str], payload: dict | None = None) -> tuple[int, str]:
    binary = 'patch-cli.exe' if sys.platform == 'win32' else 'patch-cli'
    target_root = os.environ.get('CARGO_TARGET_DIR', 'target')
    if not os.path.isabs(target_root):
        target_root = os.path.join(BASE_DIR, target_root)
    target_bin = os.path.join(BASE_DIR, 'bin', binary) if getattr(sys, 'frozen', False) else os.path.join(target_root, BUILD_MODE, binary)
    if os.path.isfile(target_bin):
        cmd = [target_bin] + args
    else:
        hint = 'Reinstall the desktop package; bundled patch-cli is missing.' if getattr(sys, 'frozen', False) else 'Build patch-cli first: cargo build ' + ('--release ' if BUILD_MODE == 'release' else '') + '-p patch-engine --bin patch-cli'
        return 1, json.dumps({'success': False, 'error': f'{hint} Expected: {target_bin}'})
    try:
        res = subprocess.run(cmd, input=json.dumps(payload) if payload is not None else None,
                             capture_output=True, text=True, encoding='utf-8', cwd=BASE_DIR,
                             timeout=120, env=desktop_environment(), creationflags=subprocess.CREATE_NO_WINDOW if sys.platform == 'win32' else 0)
    except subprocess.TimeoutExpired:
        return 1, json.dumps({'success': False, 'error': 'Desktop command timed out after 120s; inspect recovery state before retrying'})
    except OSError as error:
        return 1, json.dumps({'success': False, 'error': f'Cannot start {target_bin}: {error}'})
    out = res.stdout.strip()
    if res.returncode != 0:
        try:
            failure = json.loads(out)
        except ValueError:
            failure = None
        if not isinstance(failure, dict) or not failure.get('error'):
            detail = res.stderr.strip() or out or 'No diagnostic output'
            # Never reflect stdin credentials, including when a helper echoes them.
            for key, value in (payload or {}).items():
                if isinstance(value, str) and value:
                    detail = detail.replace(value, '[redacted]')
            out = json.dumps({'success': False, 'error': f'patch-cli exited {res.returncode}: {detail}'})
    return res.returncode, out

def verify_card(gateway: str, card: str) -> dict:
    """Read-only portal lookup: no login, activation, binding, or local writes."""
    context = ssl.create_default_context()
    ca = os.environ.get("KIRO_GATEWAY_CA_CERT")
    if ca:
        context.load_verify_locations(cafile=ca)
    request = urllib.request.Request(gateway + "/api/v1/portal/query",
        data=json.dumps({"card": card}).encode(),
        headers={"Content-Type": "application/json"}, method="POST")
    # Do not forward card secrets to redirects or a different origin.
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *args, **kwargs):
            return None
    opener = urllib.request.build_opener(NoRedirect(), urllib.request.HTTPSHandler(context=context))
    try:
        with opener.open(request, timeout=20) as response:
            result = json.loads(response.read(65537))
    except (OSError, ValueError, urllib.error.URLError) as error:
        raise ValueError("授权查询失败，请检查网络或卡密；未进行激活或接管。") from error
    if not isinstance(result, dict) or result.get("success") is not True:
        raise ValueError("无法确认卡密有效，请检查卡密。")
    if result.get("status") not in ("active", "unactivated") or result.get("isExpired") is not False:
        raise ValueError("卡密已过期、冻结或不可用，未进行接管。")
    for field in ("remainingPoints", "totalPoints", "maxDevices"):
        value = result.get(field)
        if type(value) not in (int, float) or not math.isfinite(value) or value < 0:
            raise ValueError("授权信息不完整，请稍后重新验证。")
    if not isinstance(result.get("boundDevices"), list):
        raise ValueError("授权设备信息无效，请稍后重新验证。")
    expiry = result.get("validUntil")
    if expiry is not None and (type(expiry) not in (int, float) or not math.isfinite(expiry) or expiry <= time.time() or expiry > 8640000000000):
        raise ValueError("卡密到期信息无效或已过期，未进行接管。")
    return result

def validate_gateway(value: str) -> str:
    if not isinstance(value, str) or any(c.isspace() or c in "\\\"'`" for c in value):
        raise ValueError("Invalid gateway URL")
    try:
        parsed = urllib.parse.urlsplit(value)
        _ = parsed.port
    except ValueError as error:
        raise ValueError("Invalid gateway URL") from error
    if (not parsed.hostname or parsed.username is not None or parsed.password is not None
            or "?" in value or "#" in value
            or not (parsed.scheme == 'https' or (parsed.scheme == 'http' and parsed.hostname in ('localhost', '127.0.0.1', '::1')))):
        raise ValueError("Use HTTPS (HTTP only for loopback), without credentials, query or fragment")
    return value.rstrip('/')


class SecureBridgeHandler(BaseHTTPRequestHandler):
    server_version = "KiroBridge/1.0"

    def log_message(self, format, *args):
        # Silence default noisy stdout logs
        pass

    def validate_origin(self) -> bool:
        expected = f"http://127.0.0.1:{self.server.server_port}"
        return self.headers.get("Host") == expected.removeprefix("http://") and self.headers.get("Origin") in (None, expected)

    def validate_auth(self) -> bool:
        token = self.headers.get("X-Kiro-Session-Token", "")
        return self.validate_origin() and token.isascii() and secrets.compare_digest(token, SESSION_TOKEN)

    def send_json(self, status_code: int, data: dict):
        body = json.dumps(data).encode('utf-8')
        self.send_response(status_code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("Referrer-Policy", "no-referrer")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Access-Control-Allow-Origin", "http://127.0.0.1:" + str(self.server.server_port))
        self.send_header("Access-Control-Allow-Headers", "Content-Type, X-Kiro-Session-Token")
        self.end_headers()
        self.wfile.write(body)

    def do_OPTIONS(self):
        if not self.validate_origin():
            self.send_json(403, {"success": False, "error": "Invalid origin"})
            return
        self.send_response(204)
        self.send_header("Access-Control-Allow-Origin", "http://127.0.0.1:" + str(self.server.server_port))
        self.send_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        self.send_header("Access-Control-Allow-Headers", "Content-Type, X-Kiro-Session-Token")
        self.end_headers()

    def do_GET(self):
        global LAST_HEARTBEAT
        parsed = urllib.parse.urlparse(self.path)
        path = parsed.path

        if path == "/api/ping":
            self.send_json(200, {"service": "kiro-desktop-bridge", "status": "ok"})
            return

        if path.startswith("/api/"):
            if not self.validate_auth():
                self.send_json(403, {"error": "Unauthorized: invalid session token or origin"})
                return

            if path == "/api/usage":
                code, out = run_patch_cli(["desktop-usage"])
                try:
                    data = json.loads(out)
                except ValueError:
                    data = {"success": False, "error": "Invalid usage response"}
                self.send_json(200 if code == 0 else 502, data)
                return

            if path == "/api/status":
                code, out = run_patch_cli(["status"])
                try:
                    data = json.loads(out)
                except Exception:
                    data = {"raw": out}
                if code == 0 and isinstance(data, dict):
                    try:
                        data["platform"] = sys.platform
                        data["suggested_gateway_url"] = validate_gateway(os.environ.get("KIRO_GATEWAY_URL") or DEFAULT_GATEWAY_URL)
                        data["portal_url"] = data["suggested_gateway_url"] + "/"
                    except ValueError:
                        pass
                self.send_json(200 if code == 0 else 500, data)
                return

            if path == "/api/doctor":
                try:
                    gateway = validate_gateway(urllib.parse.parse_qs(parsed.query).get("gateway_url", [""])[0] or os.environ.get("KIRO_GATEWAY_URL") or DEFAULT_GATEWAY_URL)
                except ValueError as error:
                    self.send_json(400, {"error": str(error)})
                    return
                code, out = run_patch_cli(["doctor", "--gateway-url", gateway])
                try:
                    data = json.loads(out)
                except Exception:
                    data = {"raw": out}
                self.send_json(200 if code == 0 else 500, data)
                return

            if path == "/api/memory/sample":
                code, out = run_patch_cli(["sample-memory"])
                try:
                    data = json.loads(out)
                except Exception:
                    data = {"raw": out}
                self.send_json(200 if code == 0 else 500, data)
                return

            self.send_json(404, {"error": "API not found"})
            return

        # Serve static UI files safely
        if not self.validate_origin():
            self.send_json(403, {"error": "Invalid host or origin"})
            return
        rel_path = urllib.parse.unquote(path).lstrip("/") or "index.html"
        full_path = os.path.realpath(os.path.join(UI_DIR, rel_path))
        try:
            inside_ui = os.path.commonpath([os.path.realpath(UI_DIR), full_path]) == os.path.realpath(UI_DIR)
        except ValueError:
            inside_ui = False
        if not inside_ui or not os.path.isfile(full_path):
            self.send_error(404, "File Not Found")
            return

        content_types = {
            ".html": "text/html; charset=utf-8",
            ".css": "text/css; charset=utf-8",
            ".js": "application/javascript; charset=utf-8",
            ".png": "image/png",
            ".jpg": "image/jpeg",
            ".svg": "image/svg+xml",
            ".ico": "image/x-icon",
            ".json": "application/json",
        }
        ext = os.path.splitext(full_path)[1].lower()
        mime = content_types.get(ext, "application/octet-stream")

        try:
            with open(full_path, "rb") as f:
                content = f.read()
            self.send_response(200)
            self.send_header("Content-Type", mime)
            self.send_header("Content-Security-Policy", "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'")
            self.send_header("Content-Length", str(len(content)))
            self.send_header("Cache-Control", "no-store")
            self.send_header("Referrer-Policy", "no-referrer")
            self.send_header("X-Content-Type-Options", "nosniff")
            self.end_headers()
            self.wfile.write(content)
        except Exception as e:
            self.send_error(500, f"Internal Error: {e}")

    def do_POST(self):
        global LAST_HEARTBEAT
        parsed = urllib.parse.urlparse(self.path)
        path = parsed.path

        if not path.startswith("/api/"):
            self.send_error(404, "Not Found")
            return

        if not self.validate_auth():
            self.send_json(403, {"error": "Unauthorized: invalid session token or origin"})
            return

        try:
            content_len = int(self.headers.get("Content-Length", "0"))
            if not 0 <= content_len <= 16384:
                raise ValueError("Request too large")
            payload = json.loads(self.rfile.read(content_len).decode("utf-8") or "{}")
            if not isinstance(payload, dict):
                raise ValueError("Expected JSON object")
        except (ValueError, UnicodeDecodeError):
            self.send_json(400, {"success": False, "error": "Invalid or oversized JSON request"})
            return

        if path == "/api/heartbeat":
            with HEARTBEAT_LOCK:
                LAST_HEARTBEAT = time.time()
            self.send_json(200, {"status": "alive"})
            return

        if path == "/api/verify-card":
            try:
                card = payload.get("card_key", "")
                if not isinstance(card, str) or not card.strip() or len(card) > 256:
                    raise ValueError("请输入有效卡密。")
                gateway = validate_gateway(payload.get("gateway_url") or os.environ.get("KIRO_GATEWAY_URL") or DEFAULT_GATEWAY_URL)
                result = verify_card(gateway, card.strip())
                self.send_json(200, {"success": True, "authorization": result, "gateway_url": gateway})
            except (ValueError, TypeError) as error:
                self.send_json(400, {"success": False, "error": str(error)})
            return

        if path in ("/api/activate", "/api/restore", "/api/unbind", "/api/launch"):
            card = payload.get("card_key", "")
            if not isinstance(card, str) or len(card) > 256:
                self.send_json(400, {"success": False, "error": "Invalid card key"})
                return
            if path == "/api/activate":
                if not card.strip():
                    self.send_json(400, {"success": False, "error": "Card key is required"})
                    return
                try:
                    gateway = validate_gateway(payload.get("gateway_url") if payload.get("gateway_url", "") != "" else (os.environ.get("KIRO_GATEWAY_URL") or DEFAULT_GATEWAY_URL))
                except ValueError as error:
                    self.send_json(400, {"success": False, "error": str(error)})
                    return
                confirmed = payload.get("close_kiro_confirmed", False)
                if not isinstance(confirmed, bool):
                    self.send_json(400, {"success": False, "error": "Invalid close confirmation"})
                    return
                code, out = run_patch_cli(["desktop-activate", "--gateway-url", gateway], {"card_key": card, "close_kiro_confirmed": confirmed})
            elif path == "/api/launch":
                code, out = run_patch_cli(["desktop-launch"])
            elif path == "/api/unbind":
                if not card.strip():
                    self.send_json(400, {"success": False, "error": "Re-enter card key to unbind"})
                    return
                code, out = run_patch_cli(["desktop-unbind"], {"card_key": card})
            else:
                code, out = run_patch_cli(["desktop-logout"])
            try:
                data = json.loads(out)
                if not isinstance(data, dict) or data.get("success") is not True or code != 0:
                    raise ValueError()
            except (ValueError, TypeError):
                data = {"success": False, "error": "Desktop operation failed; no success assumed"}
                try:
                    error = json.loads(out).get("error")
                    if isinstance(error, str):
                        data["error"] = error
                except (ValueError, AttributeError):
                    pass
                code = 1
            self.send_json(200 if code == 0 else 400, data)
            return

        if path == "/api/memory/trim":
            code, out = run_patch_cli(["trim-memory"])
            try:
                data = json.loads(out)
            except Exception:
                data = {"raw": out}
            self.send_json(200 if code == 0 else 500, data)
            return

        self.send_json(404, {"error": "API not found"})

def heartbeat_watchdog(server: ThreadingHTTPServer):
    # Grace period: 15s to allow Chrome to start up and load page
    time.sleep(15.0)
    while not SERVER_SHUTDOWN.is_set():
        with HEARTBEAT_LOCK:
            elapsed = time.time() - LAST_HEARTBEAT
        if elapsed > 180.0:
            print("[*] 浏览器窗口已关闭 (心跳断开)，安全关闭本地服务...")
            SERVER_SHUTDOWN.set()
            threading.Thread(target=server.shutdown).start()
            break
        time.sleep(2.0)

class DesktopWindow:
    """Expose only authenticated window controls, never filesystem or shell access."""
    SIZES = {"connect": (480, 620), "login": (480, 620), "status": (620, 820),
             "active": (620, 820), "doctor": (620, 820), "settings": (620, 820),
             "usage": (620, 820)}

    def __init__(self):
        self._window = None
        self._maximized = False

    def _allowed(self, token):
        return isinstance(token, str) and token.isascii() and secrets.compare_digest(token, SESSION_TOKEN)

    def screen(self, name, token):
        if not self._allowed(token) or name not in self.SIZES:
            return False
        if self._window:
            self._window.resize(*self.SIZES[name])
        return True

    def minimize(self, token):
        if self._allowed(token) and self._window:
            self._window.minimize()

    @staticmethod
    def _credential_store():
        # Select OS-native stores explicitly; never permit plaintext/fallback backends.
        if sys.platform == "win32":
            from keyring.backends.Windows import WinVaultKeyring
            return WinVaultKeyring()
        if sys.platform == "darwin":
            from keyring.backends.macOS import Keyring
            return Keyring()
        raise RuntimeError("Native credential storage unavailable")

    def get_remembered_card(self, token):
        if not self._allowed(token):
            return None
        try:
            return self._credential_store().get_password("Superkiro", "card")
        except Exception:
            return None

    def set_remembered_card(self, card, token):
        if not self._allowed(token) or not isinstance(card, str) or not card.strip() or len(card) > 256:
            return False
        try:
            self._credential_store().set_password("Superkiro", "card", card.strip())
            return True
        except Exception:
            return False

    def clear_remembered_card(self, token):
        if not self._allowed(token):
            return False
        try:
            store = self._credential_store()
            if store.get_password("Superkiro", "card") is not None:
                store.delete_password("Superkiro", "card")
            return True
        except Exception:
            return False

    def open_external(self, url, token):
        if not self._allowed(token):
            return False
        portal = validate_gateway(os.environ.get("KIRO_GATEWAY_URL") or DEFAULT_GATEWAY_URL) + "/"
        if url not in (portal, "https://kiro.dev/downloads/"):
            return False
        import webbrowser
        return webbrowser.open(url, new=2)

    def maximize(self, token):
        if self._allowed(token) and self._window:
            if sys.platform == "darwin":
                self._window.toggle_fullscreen()
            elif self._maximized:
                self._window.restore()
                self._maximized = False
            else:
                self._window.maximize()
                self._maximized = True

    def pick_install_path(self, token):
        if not self._allowed(token) or not self._window:
            return None
        import webview
        result = self._window.create_file_dialog(
            webview.OPEN_DIALOG, file_types=("Applications (*.app)",)
        ) if sys.platform == "darwin" else self._window.create_file_dialog(webview.FOLDER_DIALOG)
        if not result:
            return None
        code, output = run_patch_cli(["status"])
        try:
            if code != 0 or json.loads(output).get("has_snapshot"):
                return {"success": False, "error": "请先恢复原始状态，再更改安装位置。"}
        except (ValueError, AttributeError):
            return {"success": False, "error": "无法确认当前连接状态。"}
        path = os.path.realpath(result[0])
        resource = os.path.join(path, "Contents", "Resources", "app") if sys.platform == "darwin" else os.path.join(path, "resources", "app")
        try:
            with open(os.path.join(resource, "product.json"), encoding="utf-8") as file:
                product = json.load(file)
            if "kiro" not in str(product.get("nameShort", "")).lower():
                raise ValueError("not Kiro")
            destination = desktop_preferences_path()
            os.makedirs(os.path.dirname(destination), mode=0o700, exist_ok=True)
            temporary = destination + ".tmp"
            with open(temporary, "w", encoding="utf-8") as file:
                json.dump({"install_path": path}, file)
            os.replace(temporary, destination)
        except (OSError, ValueError, AttributeError):
            return {"success": False, "error": "所选位置不是有效的 Kiro 安装目录，或无法保存。"}
        return {"success": True, "path": path}

    def close(self, token):
        if self._allowed(token) and self._window:
            self._window.destroy()


def main():
    global LAST_HEARTBEAT, BUILD_MODE
    BUILD_MODE = "release" if "--dev" in sys.argv else "release"
    try:
        import webview
    except ImportError:
        raise SystemExit("Install desktop dependencies: python -m pip install -r requirements-desktop.txt")
    server = ThreadingHTTPServer(('127.0.0.1', 0), SecureBridgeHandler)
    with HEARTBEAT_LOCK:
        LAST_HEARTBEAT = time.time()
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    screen = next((arg for arg in sys.argv[1:] if arg.startswith("screen-")), "screen-01")
    url = f"http://127.0.0.1:{server.server_port}/?screen={urllib.parse.quote(screen)}#token={SESSION_TOKEN}"
    controls = DesktopWindow()
    width, height = controls.SIZES.get({'screen-04':'doctor','screen-05':'settings'}.get(screen,'connect'))
    print(f"Desktop runtime: {sys.platform}; build={BUILD_MODE}; loopback={server.server_port}", flush=True)
    try:
        controls._window = webview.create_window(
            "Superkiro", url, js_api=controls, width=width, height=height,
            min_size=(320, 300), resizable=False, frameless=True, easy_drag=False,
            shadow=True, background_color="#111314", text_select=False,
        )
        # Debug is a compiler configuration, not a different UI or an open devtools panel.
        webview.start(gui='edgechromium' if sys.platform == 'win32' else 'cocoa' if sys.platform == 'darwin' else None, debug=False, private_mode=True)
    finally:
        SERVER_SHUTDOWN.set()
        server.shutdown()
        server.server_close()
        worker.join(timeout=5)


if __name__ == '__main__':
    main()
