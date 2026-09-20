import json
import runpy
from pathlib import Path
import os
import tempfile
import unittest
from unittest.mock import Mock, patch
import run_desktop as bridge


class DesktopRuntimeTests(unittest.TestCase):
    def test_import_never_auto_trusts_bundled_ca(self):
        # Even an old CA left next to the executable must not change trust.
        with patch.dict(os.environ, {}, clear=True), patch.object(bridge.os.path, "isfile", return_value=True):
            runpy.run_path(bridge.__file__, run_name="desktop_import_test")
            self.assertNotIn("KIRO_GATEWAY_CA_CERT", os.environ)
            self.assertNotIn("NODE_EXTRA_CA_CERTS", bridge.desktop_environment())

    def test_tls_uses_public_roots_and_explicit_development_override(self):
        for ca in (None, "developer-ca.pem"):
            env = {} if ca is None else {"KIRO_GATEWAY_CA_CERT": ca}
            with self.subTest(ca=ca), patch.dict(os.environ, env, clear=True), \
                 patch.object(bridge.ssl, "create_default_context") as context, \
                 patch.object(bridge.urllib.request, "build_opener", side_effect=RuntimeError("stop before network")):
                with self.assertRaisesRegex(RuntimeError, "stop before network"):
                    bridge.verify_card("https://kiro.rent", "fixture")
                context.assert_called_once_with()
                if ca is None:
                    context.return_value.load_verify_locations.assert_not_called()
                    self.assertNotIn("KIRO_GATEWAY_CA_CERT", bridge.desktop_environment())
                else:
                    context.return_value.load_verify_locations.assert_called_once_with(cafile=ca)
                    self.assertEqual(bridge.desktop_environment()["KIRO_GATEWAY_CA_CERT"], ca)

    def test_homepage_and_override_match_browser_allowlist(self):
        controls = bridge.DesktopWindow()
        for env, homepage in (({}, "https://kiro.rent/"),
                              ({"KIRO_GATEWAY_URL": "https://dev.invalid/"}, "https://dev.invalid/")):
            with self.subTest(homepage=homepage), patch.dict(os.environ, env, clear=True), patch("webbrowser.open", return_value=True) as browser:
                self.assertTrue(controls.open_external(homepage, bridge.SESSION_TOKEN))
                browser.assert_called_once_with(homepage, new=2)
                self.assertFalse(controls.open_external("https://160.202.47.98/portal", bridge.SESSION_TOKEN))

    def test_native_packaging_embeds_ui_and_uses_rust_not_python_runtime(self):
        build = Path(bridge.__file__).parent / "scripts" / "build_desktop.py"
        module = runpy.run_path(str(build), run_name="build_test")
        with tempfile.TemporaryDirectory() as root, patch("sys.platform", "win32"), \
                patch("shutil.which", return_value="npm.cmd"), \
                patch("shutil.copy2") as copy, patch("subprocess.run") as run:
            module["main"].__globals__["ROOT"] = Path(root)
            module["main"]()
            commands = [call.args[0] for call in run.call_args_list]
            self.assertEqual(commands[:2], [["npm.cmd", "ci"], ["npm.cmd", "run", "build"]])
            self.assertIn("desktop-host", commands[2])
            self.assertIn("--locked", commands[2])
            self.assertIn("--release", commands[2])
            self.assertIn("+crt-static", run.call_args.kwargs["env"]["RUSTFLAGS"])
            self.assertEqual(copy.call_args.args[1], Path(root) / "dist" / "Superkiro.exe")
            self.assertNotIn("PyInstaller", str(commands))
            self.assertNotIn("server-ca.pem", str(commands))

    def test_native_packaging_rejects_missing_build_dependency(self):
        build = Path(bridge.__file__).parent / "scripts" / "build_desktop.py"
        module = runpy.run_path(str(build), run_name="build_test")
        with patch("sys.platform", "win32"), patch("shutil.which", return_value=None), \
                patch("subprocess.run") as run:
            with self.assertRaisesRegex(SystemExit, "Node.js/npm"):
                module["main"]()
            run.assert_not_called()

    def test_window_controls_require_current_session(self):
        controls = bridge.DesktopWindow()
        controls._window = Mock()
        self.assertFalse(controls.screen("status", "wrong"))
        controls.minimize("wrong")
        controls.maximize("wrong")
        controls.close("wrong")
        self.assertEqual(controls._window.mock_calls, [])
        self.assertTrue(controls.screen("status", bridge.SESSION_TOKEN))
        controls._window.resize.assert_called_once_with(620, 820)
        self.assertTrue(controls.screen("connect", bridge.SESSION_TOKEN))
        controls._window.resize.assert_called_with(480, 620)

    def test_platform_window_zoom_and_external_allowlist(self):
        controls = bridge.DesktopWindow()
        controls._window = Mock()
        with patch.object(bridge.sys, "platform", "win32"):
            controls.maximize(bridge.SESSION_TOKEN)
            controls.maximize(bridge.SESSION_TOKEN)
        controls._window.maximize.assert_called_once()
        controls._window.restore.assert_called_once()
        with patch.object(bridge.sys, "platform", "darwin"):
            controls.maximize(bridge.SESSION_TOKEN)
        controls._window.toggle_fullscreen.assert_called_once()
        with patch("webbrowser.open", return_value=True) as browser:
            self.assertFalse(controls.open_external("file:///secret", bridge.SESSION_TOKEN))
            self.assertFalse(controls.open_external("https://kiro.dev/downloads/", "wrong"))
            browser.assert_not_called()
            self.assertTrue(controls.open_external("https://kiro.dev/downloads/", bridge.SESSION_TOKEN))

    def test_credentials_are_authenticated_and_use_native_store(self):
        controls = bridge.DesktopWindow()
        store = Mock()
        store.get_password.return_value = "fixture-card"
        with patch.object(controls, "_credential_store", return_value=store):
            self.assertIsNone(controls.get_remembered_card("wrong"))
            self.assertFalse(controls.set_remembered_card("fixture-card", "wrong"))
            self.assertFalse(controls.clear_remembered_card("wrong"))
            store.assert_not_called()
            self.assertEqual(controls.get_remembered_card(bridge.SESSION_TOKEN), "fixture-card")
            self.assertTrue(controls.set_remembered_card("fixture-card", bridge.SESSION_TOKEN))
            store.set_password.assert_called_once_with("Superkiro", "card", "fixture-card")
            self.assertTrue(controls.clear_remembered_card(bridge.SESSION_TOKEN))
            store.delete_password.assert_called_once_with("Superkiro", "card")
        with patch.object(controls, "_credential_store", side_effect=RuntimeError("locked")):
            self.assertFalse(controls.set_remembered_card("fixture-card", bridge.SESSION_TOKEN))
            self.assertIsNone(controls.get_remembered_card(bridge.SESSION_TOKEN))

    def test_selected_installation_is_passed_to_child_only(self):
        with tempfile.TemporaryDirectory() as directory:
            file = os.path.join(directory, "preferences.json")
            with open(file, "w", encoding="utf-8") as out:
                json.dump({"install_path": directory}, out)
            with patch.object(bridge, "desktop_preferences_path", return_value=file):
                self.assertEqual(bridge.desktop_environment()["SUPERKIRO_INSTALL_DIR"], directory)
            with open(file, "w", encoding="utf-8") as out:
                out.write("invalid")
            with patch.object(bridge, "desktop_preferences_path", return_value=file):
                self.assertEqual(bridge.desktop_environment(), dict(os.environ))

    def test_frozen_binary_uses_bundled_resource(self):
        with patch.object(bridge.sys, "frozen", True, create=True), \
             patch.object(bridge.os.path, "isfile", return_value=True), \
             patch.object(bridge.subprocess, "run", return_value=Mock(returncode=0, stdout="{}", stderr="")) as run:
            bridge.run_patch_cli(["status"])
            self.assertIn(os.path.join(bridge.BASE_DIR, "bin"), run.call_args.args[0][0])
            self.assertEqual(run.call_args.kwargs["timeout"], 120)


if __name__ == "__main__":
    unittest.main()
