import json
import os
import tempfile
import unittest
from unittest.mock import Mock, patch
import run_desktop as bridge


class DesktopRuntimeTests(unittest.TestCase):
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
