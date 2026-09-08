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
BACKUP_TERMINAL_RECEIPT_MAXIMUM_BYTES = 16 * 1024 * 1024
MAXIMUM_DIAGNOSTIC_INTEGER = (1 << 64) - 1
PROTOCOL_VERSION = 1
_RECOVERY_KEY = re.compile(r"[A-Za-z0-9_-]{43}\Z")
_ERROR_CODE = re.compile(r"[a-z0-9_]{1,80}\Z")
_BACKUP_FAILURE_CATEGORIES = (
    "authentication_failed",
    "corrupt_chunk",
    "missing_chunk",
    "provider_error",
    "provider_offline",
    "provider_unavailable",
    "recovery_catalog_corrupt_chunk",
    "recovery_catalog_missing_chunk",
    "recovery_catalog_provider_error",
    "recovery_catalog_provider_offline",
    "recovery_catalog_provider_unavailable",
    "recovery_catalog_resource_limit",
    "resource_limit",
)
_PROVIDER_HEALTH = ("online", "offline", "corrupt")
_PROVIDER_AVAILABILITY = ("complete", "degraded", "offline", "corrupt", "revoked")


class ResponseTooLarge(RuntimeError):
    """A response crossed its caller-provided byte bound."""


class NodeError(RuntimeError):
    """A redacted node API failure."""

    def __init__(self, node: str, path: str, status: int, code: str | None):
        self.code = code if isinstance(code, str) and _ERROR_CODE.fullmatch(code) else None
        label = self.code or "unknown"
        super().__init__(f"{node} {path} HTTP {status} code={label}")


class DiagnosticUnavailable(RuntimeError):
    """A fixed, non-sensitive diagnostic result for malformed or inaccessible evidence."""


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


def _read_current_owner_private_file(path: str | os.PathLike[str], maximum_bytes: int) -> bytes:
    """Reads a bounded regular file without following its final path component."""
    flags = os.O_RDONLY
    for option in ("O_NOFOLLOW", "O_NONBLOCK", "O_CLOEXEC"):
        if hasattr(os, option):
            flags |= getattr(os, option)
    try:
        descriptor = os.open(Path(path), flags)
    except OSError as error:
        raise DiagnosticUnavailable() from error
    try:
        metadata = os.fstat(descriptor)
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_uid != os.getuid()
            or metadata.st_mode & 0o077
            or metadata.st_size < 0
            or metadata.st_size > maximum_bytes
        ):
            raise DiagnosticUnavailable()
        chunks: list[bytes] = []
        remaining = maximum_bytes + 1
        while remaining:
            chunk = os.read(descriptor, remaining)
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
        payload = b"".join(chunks)
    except OSError as error:
        raise DiagnosticUnavailable() from error
    finally:
        os.close(descriptor)
    if len(payload) > maximum_bytes:
        raise DiagnosticUnavailable()
    return payload


def _require_object(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise DiagnosticUnavailable()
    return value


def _require_array(value: Any) -> list[Any]:
    if not isinstance(value, list):
        raise DiagnosticUnavailable()
    return value


def _require_integer(value: Any) -> int:
    if type(value) is not int or not 0 <= value <= MAXIMUM_DIAGNOSTIC_INTEGER:
        raise DiagnosticUnavailable()
    return value


def _empty_counts(categories: tuple[str, ...]) -> dict[str, int]:
    return {category: 0 for category in categories}


def summarize_backup_response(response: Any) -> dict[str, int]:
    """Keeps only non-secret, bounded integer result counters from `/backups`."""
    body = _require_object(response)
    return {
        field: _require_integer(body.get(field))
        for field in (
            "entries",
            "bytesRead",
            "chunksStored",
            "chunksDeduplicated",
            "selectedProviders",
            "degradedFailures",
        )
    }


def summarize_backup_terminal_receipt(
    path: str | os.PathLike[str], expected_job_id: str
) -> dict[str, Any]:
    """Reads only count-level replication evidence from one owned terminal receipt."""
    try:
        payload = _read_current_owner_private_file(path, BACKUP_TERMINAL_RECEIPT_MAXIMUM_BYTES)
        receipt = _require_object(json.loads(payload))
        if receipt.get("schemaVersion") != 1 or receipt.get("jobId") != expected_job_id:
            raise DiagnosticUnavailable()
        result = _require_object(receipt.get("result"))
        replication = _require_object(result.get("replication"))
        acknowledgements = _require_object(replication.get("acknowledgements"))
        acknowledgement_objects = 0
        for locators in acknowledgements.values():
            acknowledgement_objects += len(_require_array(locators))
        catalog_acknowledgements = _require_array(
            replication.get("recoveryCatalogAcknowledgements")
        )
        health_counts = _empty_counts(_PROVIDER_HEALTH)
        for health in _require_object(replication.get("providerHealth")).values():
            if health not in health_counts:
                raise DiagnosticUnavailable()
            health_counts[health] += 1
        failure_counts = _empty_counts(_BACKUP_FAILURE_CATEGORIES)
        for failure in _require_array(replication.get("failures")):
            reason = _require_object(failure).get("reason")
            if reason not in failure_counts:
                raise DiagnosticUnavailable()
            failure_counts[reason] += 1
        return {
            "acknowledgementProviders": len(acknowledgements),
            "acknowledgedObjects": acknowledgement_objects,
            "catalogAcknowledgements": len(catalog_acknowledgements),
            "providerHealth": health_counts,
            "failureCategories": failure_counts,
        }
    except Exception:
        raise DiagnosticUnavailable() from None


def summarize_provider_verification(response: Any) -> dict[str, Any]:
    """Keeps only intact plus fixed-enum availability counts from `/backups/verify`."""
    body = _require_object(response)
    intact = body.get("intact")
    if type(intact) is not bool:
        raise DiagnosticUnavailable()
    availability_counts = _empty_counts(_PROVIDER_AVAILABILITY)
    for availability in _require_object(body.get("providerAvailability")).values():
        if availability not in availability_counts:
            raise DiagnosticUnavailable()
        availability_counts[availability] += 1
    return {"intact": intact, "providerAvailability": availability_counts}


def backup_failure_diagnostics(
    client: "DrillClient", receipt_path: str | os.PathLike[str], backup_response: Any, job_id: str
) -> dict[str, Any]:
    """Collects redacted best-effort evidence without allowing diagnostics to alter failure flow."""
    diagnostics: dict[str, Any] = {}
    try:
        response = _require_object(backup_response)
        diagnostics["backupResponse"] = summarize_backup_response(response)
        backup_id = response.get("backupId")
        snapshot_id = response.get("snapshotId")
        if not isinstance(backup_id, str) or not isinstance(snapshot_id, str):
            raise DiagnosticUnavailable()
    except Exception:
        diagnostics["backupResponse"] = {"status": "diagnostic_unavailable"}
        return diagnostics
    try:
        diagnostics["terminalReceipt"] = summarize_backup_terminal_receipt(receipt_path, job_id)
    except DiagnosticUnavailable:
        diagnostics["terminalReceipt"] = {"status": "diagnostic_unavailable"}
    try:
        verification = client.call(
            "local",
            "/api/v1/backups/verify",
            {"backupId": backup_id, "snapshotId": snapshot_id, "verifyProviders": True},
            timeout=30,
        )
        diagnostics["providerVerification"] = summarize_provider_verification(verification)
    except Exception:
        diagnostics["providerVerification"] = {"status": "diagnostic_unavailable"}
    return diagnostics


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
