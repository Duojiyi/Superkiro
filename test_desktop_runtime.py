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
