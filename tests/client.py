"""A test client for the host: MCP for tools, the connector route for the lease. Standard library only; not an SDK."""
from __future__ import annotations

import hashlib
import http.client
import json
import ssl
import threading
import uuid
from urllib.parse import urlsplit

PROTOCOL = "2025-11-25"
TARGET_META = "windows-tools/target"


class HostError(RuntimeError):
    def __init__(self, code: str, message: str, delivery: str = "unknown"):
        super().__init__(f"{code}: {message} (delivery {delivery})")
        self.code, self.delivery = code, delivery


class Client:
    """Calls for one session. `target` is the host instance and lease epoch the calls are meant for.

    The host signs its own certificate, so the client pins its SHA-256: `fingerprint` as `--install` printed it,
    or else the first one seen."""

    @classmethod
    def from_line(cls, line: str) -> Client:
        """A client for the line `--install` printed: https://<token>@<IPv4>:<port>/<certificate SHA-256>."""
        parts = urlsplit(line.strip())
        return cls(f"https://{parts.hostname}:{parts.port}", parts.username, parts.path.strip("/"))

    def __init__(self, url: str, token: str, fingerprint: str | None = None, timeout: float = 40):
        self.url, self.token, self.fingerprint, self.timeout = url.rstrip("/"), token, fingerprint, timeout
        self._context = ssl.create_default_context()
        self._context.check_hostname, self._context.verify_mode = False, ssl.CERT_NONE
        self.target: dict | None = None
        self._ready = False

    def post(self, path: str, body: dict, timeout: float | None = None) -> tuple[int, dict]:
        """A POST as the host sees it: its status and JSON, 4xx included. The token goes only to the pinned host."""
        address = urlsplit(self.url)
        connection = http.client.HTTPSConnection(address.hostname, address.port, timeout=timeout or self.timeout,
                                                 context=self._context)
        try:
            connection.connect()
            seen = hashlib.sha256(connection.sock.getpeercert(binary_form=True)).hexdigest()
            self.fingerprint = self.fingerprint or seen
            if seen != self.fingerprint:
                raise HostError("certificate_changed", "The host's certificate is not the pinned one.")
            connection.request("POST", path, json.dumps(body, ensure_ascii=False).encode(), {
                "Authorization": "Bearer " + self.token, "Content-Type": "application/json",
                "Accept": "application/json, text/event-stream", "MCP-Protocol-Version": PROTOCOL,
            })
            response = connection.getresponse()
            text = response.read().decode()
        except (OSError, http.client.HTTPException) as error:
            raise HostError("transport_error", str(error)) from None
        finally:
            connection.close()
        if response.status >= 400:
            return response.status, {}
        return response.status, json.loads(text) if text else {}

    def _mcp(self, method: str, params: dict, timeout: float | None = None) -> dict:
        status, reply = self.post("/mcp", {"jsonrpc": "2.0", "id": uuid.uuid4().hex, "method": method, "params": params}, timeout)
        if status != 200 or "result" not in reply:
            raise HostError("mcp_error", f"HTTP {status}: {str(reply)[:200]}")
        return reply["result"]

    def tools(self) -> list[dict]:
        self._initialize()
        return self._mcp("tools/list", {})["tools"]

    def call(self, tool: str, arguments: dict | None = None, timeout: float | None = None) -> dict:
        """One tool call for the selected target; the host's result, or HostError with its code."""
        self._initialize()
        params: dict = {"name": tool, "arguments": arguments or {}}
        if self.target is not None and tool != "windows_describe":
            params["_meta"] = {TARGET_META: self.target}
        return _result(self._mcp("tools/call", params, timeout).get("structuredContent") or {})

    def _initialize(self) -> None:
        if not self._ready:
            self._mcp("initialize", {"protocolVersion": PROTOCOL, "capabilities": {},
                                     "clientInfo": {"name": "windows-tools-tests", "version": "1"}})
            self.post("/mcp", {"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}})
            self._ready = True

    def describe(self) -> dict:
        result = self.call("windows_describe")
        self.target = result["target"]
        return result

    def run(self, command: str, timeout_ms: int = 30000) -> dict:
        return self.call("windows_exec_run", {"command": command, "timeoutMs": timeout_ms}, timeout_ms / 1000 + 20)

    def control(self, method: str) -> dict:
        """A lease call for the selected target. The host's reply selects the target that follows it."""
        status, reply = self.post("/connector/control", {"version": 1, "id": uuid.uuid4().hex, "method": method,
                                                         "target": self.target, "args": {}}, 5)
        if status != 200:
            raise HostError("http_error", f"HTTP {status}")
        result = _result(reply)
        if method != "control.heartbeat":
            self.target = result["target"]
        return result


def _result(envelope: dict) -> dict:
    if error := envelope.get("error"):
        raise HostError(error["code"], error["message"], error["delivery"])
    if not isinstance(envelope.get("result"), dict):
        raise HostError("protocol_error", f"No result: {str(envelope)[:200]}")
    return envelope["result"]


class Lease:
    """Hold the lease with heartbeats while the block runs; hand it back on the way out."""

    def __init__(self, client: Client, beat: float = 1.2):
        self.client, self.beat = client, beat
        self.errors: list[str] = []

    def __enter__(self) -> Client:
        self.client.describe()
        self.client.control("control.resume")
        self._stop = threading.Event()
        renewer = Client(self.client.url, self.client.token, self.client.fingerprint, 5)
        renewer.target = self.client.target
        self._thread = threading.Thread(target=self._renew, args=(renewer,), daemon=True)
        self._thread.start()
        return self.client

    def _renew(self, renewer: Client) -> None:
        while not self._stop.wait(self.beat):
            try:
                renewer.control("control.heartbeat")
            except HostError as error:
                self.errors.append(str(error))

    def __exit__(self, *exc) -> None:
        self._stop.set()
        self._thread.join()
        try:
            self.client.control("control.takeover")
        except HostError:
            pass  # already ended, as after a test that takes it over


def expect_error(callback, code: str) -> HostError:
    try:
        callback()
    except HostError as error:
        assert error.code == code, error
        return error
    raise AssertionError(f"expected {code}")
