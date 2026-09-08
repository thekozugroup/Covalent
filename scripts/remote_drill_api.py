"""Bounded HTTP and private recovery-file helpers for the remote drill."""

from __future__ import annotations

import base64
import binascii
import json
import os
import re
import ssl
import stat
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

DEFAULT_MAXIMUM_BYTES = 2 * 1024 * 1024
RECOVERY_MAXIMUM_BYTES = 24 * 1024 * 1024
RECOVERY_KIT_MAXIMUM_BYTES = 16 * 1024 * 1024
RECOVERY_KIT_MAXIMUM_ENCODED_BYTES = (4 * RECOVERY_KIT_MAXIMUM_BYTES + 2) // 3
ERROR_MAXIMUM_BYTES = 64 * 1024
PROTOCOL_VERSION = 1
_RECOVERY_KEY = re.compile(r"[A-Za-z0-9_-]{43}\Z")
_ERROR_CODE = re.compile(r"[a-z0-9_]{1,80}\Z")


class ResponseTooLarge(RuntimeError):
    """A response crossed its caller-provided byte bound."""


class NodeError(RuntimeError):
    """A redacted node API failure."""

    def __init__(self, node: str, path: str, status: int, code: str | None):
        self.code = code if isinstance(code, str) and _ERROR_CODE.fullmatch(code) else None
        label = self.code or "unknown"
        super().__init__(f"{node} {path} HTTP {status} code={label}")


class _RejectRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request: Any, file_pointer: Any, code: int, message: str,
                         headers: Any, new_url: str) -> None:
        return None


def _read_bounded(response: Any, maximum_bytes: int) -> bytes:
    declared = response.headers.get("Content-Length")
    if declared is not None:
        if not declared.isascii() or not declared.isdigit():
            response.close()
            raise ResponseTooLarge("response has an invalid Content-Length")
        if int(declared) > maximum_bytes:
            response.close()
            raise ResponseTooLarge(f"response exceeds {maximum_bytes} bytes")
    payload = response.read(maximum_bytes + 1)
    if len(payload) > maximum_bytes:
        response.close()
        raise ResponseTooLarge(f"response exceeds {maximum_bytes} bytes")
    return payload


def read_private_token(path: str | os.PathLike[str]) -> str:
    token_path = Path(path)
    flags = os.O_RDONLY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor = os.open(token_path, flags)
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_mode & 0o077:
            raise RuntimeError(f"API token is not an owner-only regular file: {token_path}")
        if metadata.st_size > 513:
            raise RuntimeError("API token exceeds 512 bytes plus an optional newline")
        chunks = []
        remaining = 514
        while remaining:
            chunk = os.read(descriptor, remaining)
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
        raw = b"".join(chunks)
    finally:
        os.close(descriptor)
    if len(raw) > 513:
        raise RuntimeError("API token exceeds 512 bytes plus an optional newline")
    token = raw.decode("ascii").strip()
    if not 32 <= len(token) <= 512 or not all(0x21 <= ord(char) <= 0x7E for char in token):
        raise RuntimeError("API token must contain 32 to 512 visible ASCII bytes")
    return token


class DrillClient:
    """Authenticated client for explicitly named drill nodes."""

    def __init__(
        self,
        nodes: dict[str, tuple[str, str | os.PathLike[str], ssl.SSLContext | None]],
    ) -> None:
        self._nodes = {
            name: (
                base,
                read_private_token(token_path),
                urllib.request.build_opener(
                    _RejectRedirects(),
                    *(() if context is None else (urllib.request.HTTPSHandler(context=context),)),
                ),
            )
            for name, (base, token_path, context) in nodes.items()
        }

    def call(
        self,
        node: str,
        path: str,
        body: Any = None,
        *,
        maximum_bytes: int = DEFAULT_MAXIMUM_BYTES,
        timeout: float = 120,
    ) -> Any:
        base, token, opener = self._nodes[node]
        encoded = None if body is None else json.dumps(body, separators=(",", ":")).encode()
        headers = {"Accept": "application/json", "Authorization": "Bearer " + token}
        if encoded is not None:
            headers["Content-Type"] = "application/json"
        request = urllib.request.Request(
            base + path,
            data=encoded,
            headers=headers,
            method="GET" if encoded is None else "POST",
        )
        try:
            with opener.open(request, timeout=timeout) as response:
                payload = _read_bounded(response, maximum_bytes)
        except urllib.error.HTTPError as error:
            try:
                raw_error = _read_bounded(error, ERROR_MAXIMUM_BYTES)
                decoded = json.loads(raw_error or b"{}")
                code = decoded.get("code") if isinstance(decoded, dict) else None
            except (ResponseTooLarge, UnicodeDecodeError, json.JSONDecodeError):
                code = None
            finally:
                error.close()
            raise NodeError(node, path, error.code, code) from None
        return json.loads(payload or b"null")


def decode_recovery_export(response: Any) -> tuple[bytes, bytes]:
    if not isinstance(response, dict) or set(response) != {
        "protocolVersion",
        "recoveryKit",
        "recoveryKey",
    }:
        raise RuntimeError("recovery export has an unexpected shape")
    if type(response["protocolVersion"]) is not int or response["protocolVersion"] != PROTOCOL_VERSION:
        raise RuntimeError("recovery export has an invalid protocol version")
    encoded_kit = response["recoveryKit"]
    recovery_key = response["recoveryKey"]
    if not isinstance(encoded_kit, str) or not encoded_kit:
        raise RuntimeError("recovery kit is missing")
    if len(encoded_kit) > RECOVERY_KIT_MAXIMUM_ENCODED_BYTES:
        raise RuntimeError("encoded recovery kit is outside its byte bound")
    if not isinstance(recovery_key, str) or not _RECOVERY_KEY.fullmatch(recovery_key):
        raise RuntimeError("recovery key is not canonical base64url")
    padding = "=" * ((4 - len(encoded_kit) % 4) % 4)
    try:
        kit = base64.b64decode(encoded_kit + padding, altchars=b"-_", validate=True)
    except (binascii.Error, ValueError, UnicodeEncodeError):
        raise RuntimeError("recovery kit is not canonical base64url") from None
    if not kit or len(kit) > RECOVERY_KIT_MAXIMUM_BYTES:
        raise RuntimeError("decoded recovery kit is outside its byte bound")
    if base64.urlsafe_b64encode(kit).decode().rstrip("=") != encoded_kit:
        raise RuntimeError("recovery kit base64url is not canonical")
    try:
        decoded_key = base64.b64decode(recovery_key + "=", altchars=b"-_", validate=True)
    except (binascii.Error, ValueError, UnicodeEncodeError):
        raise RuntimeError("recovery key is not canonical base64url") from None
    if len(decoded_key) != 32 or base64.urlsafe_b64encode(decoded_key).decode().rstrip("=") != recovery_key:
        raise RuntimeError("recovery key is not canonical base64url")
    return kit, recovery_key.encode("ascii")


def write_private(path: str | os.PathLike[str], payload: bytes) -> None:
    destination = Path(path)
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor = os.open(destination, flags, 0o600)
    try:
        os.fchmod(descriptor, 0o600)
        with os.fdopen(descriptor, "wb", closefd=False) as output:
            output.write(payload)
            output.flush()
            os.fsync(output.fileno())
    finally:
        os.close(descriptor)
    metadata = destination.lstat()
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_mode & 0o077 or metadata.st_size != len(payload):
        raise RuntimeError(f"private file contract failed: {destination}")
