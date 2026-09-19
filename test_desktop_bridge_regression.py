"""Isolated desktop bridge regressions; never invokes Kiro or patch-cli."""
import io
import http.client
import json
import threading
import unittest
from unittest.mock import patch

import run_desktop as bridge


class BridgeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = bridge.ThreadingHTTPServer(("127.0.0.1", 0), bridge.SecureBridgeHandler)
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()
        cls.thread.join(5)

    def test_usage_is_authenticated_and_read_only(self):
        with patch.object(bridge, "run_patch_cli", return_value=(0, '{"success":true,"usage":{}}')) as cli:
            self.assertEqual(self.request("/api/usage", authenticated=False)[0], 403)
            cli.assert_not_called()
            self.assertEqual(self.request("/api/usage")[0], 200)
            cli.assert_called_once_with(["desktop-usage"])
        with patch.object(bridge, "run_patch_cli", return_value=(1, '{"success":false,"error":"offline"}')):
            self.assertEqual(self.request("/api/usage")[0], 502)

    def test_verify_is_read_only_and_authenticated(self):
        payload = {"card_key": "test-secret", "gateway_url": "https://gateway.invalid"}
        with patch.object(bridge, "verify_card", return_value={"status": "active"}) as verify, patch.object(bridge, "run_patch_cli") as cli:
            self.assertEqual(self.request("/api/verify-card", "POST", payload, authenticated=False)[0], 403)
            verify.assert_not_called()
            code, body, _ = self.request("/api/verify-card", "POST", payload)
            self.assertEqual(code, 200)
            self.assertNotIn("test-secret", body.decode())
            verify.assert_called_once_with("https://gateway.invalid", "test-secret")
            cli.assert_not_called()
        with patch.object(bridge, "verify_card", side_effect=ValueError("卡密不可用")), patch.object(bridge, "run_patch_cli") as cli:
            self.assertEqual(self.request("/api/verify-card", "POST", payload)[0], 400)
            cli.assert_not_called()

    def test_portal_lookup_rejects_invalid_authorization(self):
        good = {"success": True, "status": "active", "isExpired": False,
                "remainingPoints": 100, "totalPoints": 200, "maxDevices": 2,
                "boundDevices": [], "validUntil": 2000000000}
        cases = [good, {**good, "status": "unactivated", "validUntil": None}]
        cases += [{**good, "status": status} for status in ("expired", "frozen", "banned", "voided")]
        cases += [{**good, "isExpired": True}, {**good, "validUntil": 1}, {**good, "remainingPoints": None}, {}]
        for index, result in enumerate(cases):
            with self.subTest(index=index), patch.object(bridge.urllib.request, "build_opener") as factory:
                factory.return_value.open.return_value = io.BytesIO(json.dumps(result).encode())
                if index < 2:
                    self.assertEqual(bridge.verify_card("https://gateway.invalid", "test-secret"), result)
                else:
                    with self.assertRaises(ValueError):
                        bridge.verify_card("https://gateway.invalid", "test-secret")
                request = factory.return_value.open.call_args.args[0]
                self.assertEqual(request.full_url, "https://gateway.invalid/api/v1/portal/query")
                self.assertEqual(json.loads(request.data), {"card": "test-secret"})
                self.assertIsNone(factory.call_args.args[0].redirect_request(None, None, None, None, None, None))

    def test_bundled_gateway_without_launcher_environment(self):
        with patch.dict(bridge.os.environ, {}, clear=True), patch.object(bridge, "run_patch_cli", return_value=(0, '{"success":true}')) as cli:
            code, body, _ = self.request("/api/status")
            self.assertEqual(code, 200)
            self.assertEqual(json.loads(body)["suggested_gateway_url"], bridge.DEFAULT_GATEWAY_URL)
            for value in ({"card_key": "test-card"}, {"card_key": "test-card", "gateway_url": ""}):
                code, _, _ = self.request("/api/activate", "POST", value)
                self.assertEqual(code, 200)
                self.assertEqual(cli.call_args.args[0], ["desktop-activate", "--gateway-url", bridge.DEFAULT_GATEWAY_URL])
            code, _, _ = self.request("/api/doctor")
            self.assertEqual(code, 200)
            self.assertEqual(cli.call_args.args[0], ["doctor", "--gateway-url", bridge.DEFAULT_GATEWAY_URL])

    def request(self, path, method="GET", payload=None, headers=None, authenticated=True):
        conn = http.client.HTTPConnection("127.0.0.1", self.server.server_port, timeout=5)
        defaults = {"Origin": f"http://127.0.0.1:{self.server.server_port}"}
        if authenticated:
            defaults["X-Kiro-Session-Token"] = bridge.SESSION_TOKEN
        defaults.update(headers or {})
        body = json.dumps(payload) if payload is not None else None
        conn.request(method, path, body=body, headers=defaults)
        response = conn.getresponse()
        result = response.status, response.read(), dict(response.getheaders())
        conn.close()
        return result

    def test_token_and_origin_are_required(self):
        with patch.object(bridge, "run_patch_cli") as cli:
            for headers, authenticated, path in [
                ({}, False, "/api/status"),
                ({"X-Kiro-Session-Token": "wrong"}, True, "/api/status"),
                ({"Origin": "https://evil.invalid"}, True, "/api/status"),
                ({"Host": "evil.invalid"}, True, "/api/status"),
                ({}, False, "/api/status?token=" + bridge.SESSION_TOKEN),
            ]:
                self.assertEqual(self.request(path, headers=headers, authenticated=authenticated)[0], 403)
            cli.assert_not_called()

    def test_deployment_gateway_is_suggested_not_claimed_applied(self):
        with patch.dict(bridge.os.environ, {"KIRO_GATEWAY_URL": "https://160.202.47.98"}), patch.object(bridge, "run_patch_cli", return_value=(0, '{"gateway_url":null}')):
            status, body, _ = self.request("/api/status")
            self.assertEqual(status, 200)
            self.assertEqual(json.loads(body)["suggested_gateway_url"], "https://160.202.47.98")
            self.assertIsNone(json.loads(body)["gateway_url"])

    def test_static_paths_and_headers(self):
        status, body, headers = self.request("/")
        self.assertEqual(status, 200)
        self.assertIn(b"html", body)
        self.assertEqual(headers["Cache-Control"], "no-store")
        self.assertEqual(headers["Referrer-Policy"], "no-referrer")
        for path in ["/../Cargo.toml", "/%2e%2e/Cargo.toml", "/%2e%2e%5cCargo.toml"]:
            self.assertEqual(self.request(path)[0], 404)
        self.assertEqual(self.request("/", headers={"Host": "evil.invalid"})[0], 403)

    def test_card_is_passed_on_stdin_and_only_success_is_accepted(self):
        payload = {"gateway_url": "https://gateway.invalid/", "card_key": "test-card-secret"}
        with patch.object(bridge, "run_patch_cli", return_value=(0, '{"success":true}')) as cli:
            self.assertEqual(self.request("/api/activate", "POST", payload)[0], 200)
            cli.assert_called_once_with(["desktop-activate", "--gateway-url", "https://gateway.invalid"],
                                        {"card_key": "test-card-secret", "close_kiro_confirmed": False})
        for result in [(1, '{"success":true}'), (0, "not json"), (0, '{}'), (0, '[]'), (1, '{"error":"blocked"}')]:
            with patch.object(bridge, "run_patch_cli", return_value=result):
                status, body, _ = self.request("/api/activate", "POST", payload)
                self.assertEqual(status, 400)
                self.assertFalse(json.loads(body)["success"])

    def test_validation_never_invokes_cli(self):
        with patch.object(bridge, "run_patch_cli") as cli:
            for gateway in ["http://gateway.invalid", "https://u:p@gateway.invalid", "https://gateway.invalid?q=a",
                            "https://gateway.invalid#x", "https://gateway.invalid/'", None]:
                self.assertEqual(self.request("/api/activate", "POST", {"gateway_url": gateway, "card_key": "test"})[0], 400)
            for payload in [[], "text", {"card_key": 7}, {"card_key": "x" * 257}]:
                self.assertEqual(self.request("/api/unbind", "POST", payload)[0], 400)
            self.assertEqual(self.request("/api/unbind", "POST", {"card_key": ""})[0], 400)
            self.assertEqual(self.request("/api/restore", "POST", {"x": "x" * 17000})[0], 400)
            cli.assert_not_called()

    def test_restore_and_unbind_routes(self):
        with patch.object(bridge, "run_patch_cli", return_value=(0, '{"success":true}')) as cli:
            self.assertEqual(self.request("/api/restore", "POST", {})[0], 200)
            cli.assert_called_with(["desktop-logout"])
            self.assertEqual(self.request("/api/unbind", "POST", {"card_key": "test"})[0], 200)
            cli.assert_called_with(["desktop-unbind"], {"card_key": "test"})

    def test_launch_requires_local_session_and_propagates_failure(self):
        with patch.object(bridge, "run_patch_cli", return_value=(0, '{"success":true}')) as cli:
            self.assertEqual(self.request("/api/launch", "POST", {}, authenticated=False)[0], 403)
            cli.assert_not_called()
            self.assertEqual(self.request("/api/launch", "POST", {})[0], 200)
            cli.assert_called_once_with(["desktop-launch"])
        with patch.object(bridge, "run_patch_cli", return_value=(1, '{"error":"Activate a card first"}')):
            status, body, _ = self.request("/api/launch", "POST", {})
            self.assertEqual(status, 400)
            self.assertFalse(json.loads(body)["success"])

    def test_doctor_uses_selected_gateway(self):
        with patch.object(bridge, "run_patch_cli", return_value=(0, '{"items":[]}')) as cli:
            self.assertEqual(self.request("/api/doctor?gateway_url=https%3A%2F%2Fgateway.invalid")[0], 200)
            cli.assert_called_once_with(["doctor", "--gateway-url", "https://gateway.invalid"])
        with patch.object(bridge, "run_patch_cli") as cli:
            self.assertEqual(self.request("/api/doctor?gateway_url=http%3A%2F%2Fevil.invalid")[0], 400)
            self.assertEqual(self.request("/api/activate", "POST", {"gateway_url":"https://gateway.invalid", "card_key":"  "})[0], 400)
            cli.assert_not_called()

    def test_gateway_url_policy(self):
        for url in ["http://127.0.0.1:1234", "http://[::1]:1234", "https://gateway.invalid/api"]:
            self.assertEqual(bridge.validate_gateway(url), url)
        for url in ["http://192.168.1.1", "https://gateway.invalid:999999", "https://gateway.invalid/\\x"]:
            with self.assertRaises(ValueError):
                bridge.validate_gateway(url)


if __name__ == "__main__":
    unittest.main()
