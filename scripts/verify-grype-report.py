#!/usr/bin/env python3
"""Fail closed unless a bounded Grype report is bound to one exact image ID."""

from __future__ import annotations

import json
import re
import stat
import sys
from pathlib import Path

MAX_REPORT_BYTES = 128 * 1024 * 1024
IMAGE_ID_RE = re.compile(r"sha256:[0-9a-f]{64}\Z")


def fail(message: str) -> "None":
    raise ValueError(message)


def safe_field(value: object, limit: int = 96) -> str:
    text = value if isinstance(value, str) else "unknown"
    cleaned = "".join(character if " " <= character <= "~" else "?" for character in text)
    return cleaned if len(cleaned) <= limit else cleaned[: limit - 3] + "..."


def summarize(report: dict[str, object]) -> None:
    descriptor = report["descriptor"]
    assert isinstance(descriptor, dict)
    database = descriptor["db"]
    assert isinstance(database, dict)
    status = database.get("status", database)
    status = status if isinstance(status, dict) else {}
    schema = status.get("schemaVersion", status.get("schema", "unknown"))
    built = status.get("built", "unknown")
    matches = report["matches"]
    assert isinstance(matches, list)

    counts: dict[str, int] = {}
    findings: dict[str, list[tuple[object, object, object, object, object]]] = {
        "high/critical": [],
        "medium": [],
    }
    for match in matches:
        if not isinstance(match, dict):
            severity = "Malformed"
            vulnerability: dict[str, object] = {}
            artifact: dict[str, object] = {}
        else:
            raw_vulnerability = match.get("vulnerability")
            raw_artifact = match.get("artifact")
            vulnerability = raw_vulnerability if isinstance(raw_vulnerability, dict) else {}
            artifact = raw_artifact if isinstance(raw_artifact, dict) else {}
            raw_severity = safe_field(vulnerability.get("severity"), 24).lower()
            severity = {
                "negligible": "Negligible",
                "low": "Low",
                "medium": "Medium",
                "high": "High",
                "critical": "Critical",
                "unknown": "Unknown",
            }.get(raw_severity, "Other")
        counts[severity] = counts.get(severity, 0) + 1
        if severity.lower() in {"medium", "high", "critical"}:
            fix = vulnerability.get("fix")
            fix = fix if isinstance(fix, dict) else {}
            versions = fix.get("versions", [])
            versions = versions if isinstance(versions, list) else []
            group = "medium" if severity == "Medium" else "high/critical"
            findings[group].append(
                (
                    vulnerability.get("id"),
                    artifact.get("name"),
                    artifact.get("version"),
                    artifact.get("type"),
                    versions,
                )
            )

    count_text = ",".join(f"{key}:{counts[key]}" for key in sorted(counts)) or "none"
    print(
        "Grype report summary: "
        f"scanner=grype/0.117.0 dbSchema={safe_field(schema)} dbBuilt={safe_field(built)} "
        f"matches={len(matches)} severities={count_text}"
    )
    # Bound each severity group independently so medium diagnostics cannot
    # crowd a blocking finding out of the retained job log.
    for group, items in findings.items():
        for vulnerability_id, name, version, package_type, versions in items[:30]:
            fixed_values = [safe_field(item, 48) for item in versions[:5]]
            fixed = ",".join(fixed_values) if fixed_values else "none"
            if len(versions) > 5:
                fixed += f",...(+{len(versions) - 5})"
            print(
                f"Grype {group}: "
                f"id={safe_field(vulnerability_id)} package={safe_field(name)} "
                f"version={safe_field(version)} type={safe_field(package_type)} fixed={fixed}"
            )
        if len(items) > 30:
            print(f"Grype {group}: omitted={len(items) - 30}")


def validate(path: Path, expected_image_id: str) -> dict[str, object]:
    if not IMAGE_ID_RE.fullmatch(expected_image_id):
        fail("expected image ID is not an exact sha256 digest")
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
        fail("Grype report must be one regular, single-linked file")
    if metadata.st_size <= 0 or metadata.st_size > MAX_REPORT_BYTES:
        fail("Grype report size is outside the allowed range")
    with path.open("rb") as handle:
        report = json.load(handle)
        if handle.read(1):
            fail("Grype report has trailing bytes")
    if not isinstance(report, dict):
        fail("Grype report root must be an object")

    descriptor = report.get("descriptor")
    if not isinstance(descriptor, dict):
        fail("Grype report descriptor is missing")
    if descriptor.get("name") != "grype" or descriptor.get("version") != "0.117.0":
        fail("Grype report was not produced by pinned Grype 0.117.0")
    if not isinstance(descriptor.get("db"), dict) or not descriptor["db"]:
        fail("Grype report does not identify its vulnerability database")

    source = report.get("source")
    target = source.get("target") if isinstance(source, dict) else None
    if not isinstance(source, dict) or source.get("type") != "image" or not isinstance(target, dict):
        fail("Grype report source is not an image")
    # Grype v0.117.0 strips the explicit docker: source selector before it
    # passes the reference to Syft, which preserves that reference as userInput.
    # The source selector is enforced by our invocation; both recorded values
    # must still be the exact immutable Docker image ID.
    # https://github.com/anchore/grype/blob/v0.117.0/grype/pkg/syft_provider.go
    if target.get("userInput") != expected_image_id:
        fail("Grype report input does not match the exact requested image ID")
    if target.get("imageID") != expected_image_id:
        fail("Grype report resolved a different image ID")

    if not isinstance(report.get("matches"), list):
        fail("Grype report matches are missing")
    ignored = report.get("ignoredMatches", [])
    if not isinstance(ignored, list) or ignored:
        fail("Grype report contains suppressed findings")
    return report


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print(f"usage: {argv[0]} REPORT IMAGE_ID", file=sys.stderr)
        return 2
    try:
        report = validate(Path(argv[1]), argv[2])
    except (OSError, UnicodeError, json.JSONDecodeError, ValueError) as error:
        print(f"invalid Grype report: {error}", file=sys.stderr)
        return 1
    summarize(report)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
