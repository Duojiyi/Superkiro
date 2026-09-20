"""Offline SSH transport regressions; also run with python -O (no network)."""
import base64
import hashlib
import importlib.util
from pathlib import Path
import types
import unittest
from unittest.mock import MagicMock, patch


class SshTransportTests(unittest.TestCase):
    def setUp(self):
        self.paramiko = types.ModuleType('paramiko')
        self.transport = MagicMock()
        self.paramiko.Transport = MagicMock(return_value=self.transport)
        self.paramiko.SSHClient = MagicMock()
        smoke = types.ModuleType('test_single_server_smoke')
        smoke.decode_frames = lambda data: data
        spec = importlib.util.spec_from_file_location('ssh_transport_under_test', Path(__file__).resolve().parents[1] / 'test_deployed_server.py')
        self.module = importlib.util.module_from_spec(spec)
        with patch.dict('sys.modules', {'paramiko': self.paramiko, 'requests': types.ModuleType('requests'), 'test_single_server_smoke': smoke}):
            spec.loader.exec_module(self.module)
        self.transport.get_remote_server_key.return_value.asbytes.return_value = b'pinned-host'
        self.module.FINGERPRINT = base64.b64encode(hashlib.sha256(b'pinned-host').digest()).decode().rstrip('=')
        self.sock = MagicMock()
        self.connection = patch.object(self.module.socket, 'create_connection', return_value=self.sock)
        self.connection.start()
        self.addCleanup(self.connection.stop)

    def test_matching_host_authenticates_and_retains_connection(self):
        client = self.module.connect('fixture-password', use_proxy=False)
        self.transport.auth_password.assert_called_once_with('root', 'fixture-password')
        self.assertIs(client._transport, self.transport)
        self.transport.close.assert_not_called()

    def test_changed_host_refuses_before_credentials_and_closes(self):
        self.module.FINGERPRINT = 'untrusted'
        with self.assertRaisesRegex(RuntimeError, 'SSH host key changed'):
            self.module.connect('fixture-password', use_proxy=False)
        self.transport.auth_password.assert_not_called()
        self.transport.close.assert_called_once()

    def test_rejected_proxy_is_not_an_ssh_tunnel(self):
        self.sock.recv.side_effect = [bytes([byte]) for byte in b'HTTP/1.1 403 Forbidden\r\n\r\n']
        with self.assertRaisesRegex(RuntimeError, 'SSH proxy rejected tunnel'):
            self.module.connect('fixture-password')
        self.paramiko.Transport.assert_not_called()
        self.sock.close.assert_called_once()

    def test_authentication_failure_closes_transport(self):
        self.transport.auth_password.side_effect = RuntimeError('authentication rejected')
        with self.assertRaisesRegex(RuntimeError, 'authentication rejected'):
            self.module.connect('fixture-password', use_proxy=False)
        self.transport.close.assert_called_once()


if __name__ == '__main__':
    unittest.main()
