"""Offline HTTP contracts for the bounded proposer identity, not ML runs."""

import importlib.util
import json
import os
import tempfile
import threading
import unittest
import urllib.error
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("prime_proposer", Path(__file__).with_name("entrypoint.py"))
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


class MachineAccessTests(unittest.TestCase):
    def setUp(self):
        self.lifetime = 1200
        self.leaked_requests = []
        self.tools = ["list_campaigns", "list_experiments", "list_reports", "list_drives", "get_report", "get_manifest"]
        case = self

        class Endpoint(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def respond(self, status, value):
                data = json.dumps(value).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def do_GET(self):
                case.leaked_requests.append(self.path)
                self.respond(200, {})

            def do_POST(self):
                body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
                if self.path == "/redirect":
                    self.send_response(307)
                    self.send_header("Location", "/leaked-credential")
                    self.end_headers()
                    return
                if self.path == "/leaked-credential":
                    case.leaked_requests.append(self.path)
                    self.respond(200, {})
                    return
                if self.path == "/token":
                    form = urllib.parse.parse_qs(body.decode())
                    if (form.get("client_secret") != ["fixture-secret"]
                            or form.get("grant_type") != ["client_credentials"]
                            or form.get("client_id") != ["research-reader"]
                            or form.get("resource") != ["https://athena.example.test"]):
                        self.respond(401, {"error": "invalid_client"})
                        return
                    self.respond(200, {"access_token": "fixture-reader-token", "token_type": "Bearer", "expires_in": case.lifetime})
                    return
                if self.headers.get("Authorization") != "Bearer fixture-reader-token":
                    self.respond(401, {"error": "unauthorized"})
                    return
                request = json.loads(body)
                result = ({"tools": [{"name": name} for name in case.tools]}
                          if request["method"] == "tools/list" else {"capabilities": {}})
                self.respond(200, {"jsonrpc": "2.0", "id": request["id"], "result": result})

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Endpoint)
        self.thread = threading.Thread(target=self.server.serve_forever, kwargs={"poll_interval": 0.01}, daemon=True)
        self.thread.start()
        self.addCleanup(self.close_server)
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.secret = Path(self.directory.name) / "client-secret"
        self.secret.write_text("fixture-secret")
        self.base = f"http://127.0.0.1:{self.server.server_port}"
        self.environment = patch.dict(os.environ, {
            "MCP_OAUTH_TOKEN_URL": self.base + "/token",
            "MCP_OAUTH_CLIENT_ID": "research-reader",
            "MCP_OAUTH_RESOURCE": "https://athena.example.test",
            "MCP_OAUTH_CLIENT_SECRET_FILE": str(self.secret),
            "ATHENA_MCP_URL": self.base + "/mcp",
        }, clear=True)
        self.environment.start()
        self.addCleanup(self.environment.stop)

    def close_server(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=1)

    def test_scoped_identity_can_discover_research_memory(self):
        token = runner.mcp_bearer(900)
        runner.verify_mcp(token)
        with self.assertRaises(urllib.error.HTTPError) as error:
            runner.verify_mcp("wrong-identity")
        self.assertEqual(error.exception.code, 401)

    def test_short_token_cannot_expire_halfway_through_a_job(self):
        self.lifetime = 300
        with self.assertRaisesRegex(RuntimeError, "bounded Job lifetime"):
            runner.mcp_bearer(900)

    def test_wrong_client_secret_does_not_fall_back_to_anonymous(self):
        self.secret.write_text("wrong-secret")
        with self.assertRaises(urllib.error.HTTPError) as error:
            runner.mcp_bearer(900)
        self.assertEqual(error.exception.code, 401)

    def test_bearer_is_not_forwarded_through_a_redirect(self):
        os.environ["ATHENA_MCP_URL"] = self.base + "/redirect"
        with self.assertRaises(urllib.error.HTTPError):
            runner.verify_mcp("fixture-reader-token")
        self.assertEqual(self.leaked_requests, [])

    def test_client_secret_is_not_forwarded_through_a_redirect(self):
        os.environ["MCP_OAUTH_TOKEN_URL"] = self.base + "/redirect"
        with self.assertRaises(urllib.error.HTTPError):
            runner.mcp_bearer(900)
        self.assertEqual(self.leaked_requests, [])

    def test_missing_prior_art_access_stops_the_proposer(self):
        self.tools.remove("list_reports")
        with self.assertRaisesRegex(RuntimeError, "required research-memory tools"):
            runner.verify_mcp("fixture-reader-token")

    def test_partial_credentials_cannot_silently_disable_authentication(self):
        del os.environ["MCP_OAUTH_CLIENT_ID"]
        with self.assertRaisesRegex(RuntimeError, "incomplete"):
            runner.mcp_bearer(900)


if __name__ == "__main__":
    unittest.main()
