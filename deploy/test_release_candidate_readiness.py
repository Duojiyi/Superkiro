"""Regression tests for the public readiness response contract."""
import json
import unittest
from contextlib import ExitStack
from unittest.mock import patch

from requests import Response

from deploy import release_candidate


def response(status, content_type, payload):
    result = Response()
    result.status_code = status
    result.headers["Content-Type"] = content_type
    result._content = payload if isinstance(payload, bytes) else json.dumps(payload).encode()
    return result


class ExternalReadinessContractTests(unittest.TestCase):
    def setUp(self):
        self.stack = ExitStack()
        self.addCleanup(self.stack.close)
        sessions = self.stack.enter_context(patch.object(release_candidate.requests, "Session"))
        self.http = sessions.return_value.__enter__.return_value
        self.stack.enter_context(patch.object(release_candidate.time, "sleep"))
        self.health = response(200, "application/json", {
            "status": "healthy", "service": "kiro-byok-gateway",
            "persistenceReady": True, "version": "0.1.0", "uptimeSecs": 10,
        })
        self.admin = response(200, "text/html", b"<!doctype html><title>Admin login</title>")
        self.api = response(401, "application/json", {"error": "Unauthorized"})
        self.post = response(401, "application/x-amz-json-1.1", {
            "__type": "MissingAuthenticationTokenException",
            "message": "Missing or malformed Authorization header",
        })
        self.http.get.side_effect = lambda url, **kwargs: {
            "https://kiro.rent/healthz": self.health,
            "https://kiro.rent/admin/": self.admin,
            "https://kiro.rent/api/v1/admin/stats": self.api,
        }[url]
        self.http.post.return_value = self.post

    def test_gateway_aws_json_unauthorized_root_is_ready(self):
        release_candidate.external_readiness()
        self.http.post.assert_called_once_with(
            "https://kiro.rent/", json={}, timeout=8, allow_redirects=False
        )
        self.assertFalse(self.http.trust_env)

    def test_aws_json_with_charset_is_ready(self):
        self.post.headers["Content-Type"] = "application/x-amz-json-1.1; charset=utf-8"
        release_candidate.external_readiness()

    def test_aws_json_1_0_remains_supported(self):
        self.post.headers["Content-Type"] = "application/x-amz-json-1.0"
        release_candidate.external_readiness()

    def test_admin_api_must_require_authentication(self):
        self.api.status_code = 200
        with self.assertRaises(RuntimeError):
            release_candidate.external_readiness()

    def test_browser_login_must_not_issue_basic_challenge(self):
        self.admin.headers['WWW-Authenticate'] = 'Basic realm="Admin"'
        with self.assertRaises(RuntimeError):
            release_candidate.external_readiness()

    def test_legacy_basic_mode_remains_explicit(self):
        self.admin = response(401, 'text/plain', b'Unauthorized')
        release_candidate.external_readiness(browser_login=False)

    def test_root_server_error_is_rejected(self):
        self.post.status_code = 500
        with self.assertRaises(RuntimeError):
            release_candidate.external_readiness()

    def test_standard_json_root_remains_supported(self):
        self.post.headers["Content-Type"] = "application/json; charset=utf-8"
        release_candidate.external_readiness()

    def test_non_json_root_is_rejected(self):
        self.post.headers["Content-Type"] = "text/html"
        with self.assertRaises(RuntimeError):
            release_candidate.external_readiness()

    def test_invalid_json_root_is_rejected(self):
        self.post = response(401, "application/x-amz-json-1.1", b"not-json")
        self.http.post.return_value = self.post
        with self.assertRaises(RuntimeError):
            release_candidate.external_readiness()

    def test_health_failure_is_rejected(self):
        self.health.status_code = 503
        with self.assertRaises(RuntimeError):
            release_candidate.external_readiness()


if __name__ == "__main__":
    unittest.main()
