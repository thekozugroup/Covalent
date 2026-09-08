#!/bin/sh
# Collect fail-closed Android target dependency and govulncheck evidence for pinned Syncthing.
set -eu
umask 077

usage() {
  echo "usage: $0 /exact/syncthing/checkout /new/private/output" >&2
}

test "$#" = 2 || { usage; exit 2; }
source_dir=$1
output_root=$2

syncthing_commit=946e2b83a1f6c6ae119427c09e0a5802940b82ff
syncthing_gomod_sha=a129d6ae9cf20593fab4b1fb04ac09b176c4942d3a4bec9394f9c888fe2d1bd1
syncthing_gosum_sha=7e9606117eca33e9263181a3d0141e403c940c55022a061d8ed9e22d4bda2acd
go_version='go version go1.26.7 '
govuln_module=golang.org/x/vuln/cmd/govulncheck
govuln_version=v1.7.0
govuln_zip_hash='h1:4MQBuhmXbz2uepNJrf3v+aaZLGDqw1JluwYboegA1qg='
official_db=https://vuln.go.dev
expected_ndk=27.1.12297006
max_output_bytes=3221225472
max_output_entries=200000
max_single_file_blocks=4194304
command_timeout_seconds=900

fail() { echo "error: $*" >&2; exit 1; }
command -v git >/dev/null 2>&1 || fail "git is required"
command -v python3 >/dev/null 2>&1 || fail "python3 is required"
command -v shasum >/dev/null 2>&1 || fail "shasum is required"
timeout_bin=$(command -v timeout || command -v gtimeout || true)
test -n "$timeout_bin" || fail "GNU timeout or gtimeout is required"
go_bin=$(command -v go || true)
test -n "$go_bin" || fail "Go 1.26.7 is required on PATH"

test -d "$source_dir" || fail "Syncthing source checkout is missing"
source_dir=$(CDPATH='' cd -- "$source_dir" && pwd -P)
{ test -d "$source_dir/.git" || test -f "$source_dir/.git"; } ||
  fail "source must be a git checkout"
test "$(git -C "$source_dir" rev-parse HEAD)" = "$syncthing_commit" ||
  fail "source checkout is not the pinned Syncthing commit"
test -z "$(git -C "$source_dir" status --porcelain=v1 --untracked-files=all)" ||
  fail "source checkout must be completely clean"
test "$(shasum -a 256 "$source_dir/go.mod" | awk '{print $1}')" = "$syncthing_gomod_sha" ||
  fail "pinned go.mod hash differs"
test "$(shasum -a 256 "$source_dir/go.sum" | awk '{print $1}')" = "$syncthing_gosum_sha" ||
  fail "pinned go.sum hash differs"
case $("$go_bin" version) in
  "$go_version"*) ;;
  *) fail "Go must report exactly 1.26.7" ;;
esac

test ! -e "$output_root" || fail "output must be a new path"
test ! -L "$output_root" || fail "output must be a new path"
output_parent=$(dirname -- "$output_root")
test -d "$output_parent" || fail "output parent must already exist"
mkdir "$output_root"
output_root=$(CDPATH='' cd -- "$output_root" && pwd -P)
case "$output_root/" in "$source_dir/"*) fail "output must be outside source checkout" ;; esac
chmod 700 "$output_root"
printf '%s\n' syncthing-android-target-supply-chain-v1 > "$output_root/.owned"
reports=$output_root/reports
cache=$output_root/cache
bindir=$output_root/bin
mkdir "$reports" "$reports/official-osv" "$cache" "$bindir"
cat > "$reports/collection-status.json" <<'EOF'
{
  "schemaVersion": 1,
  "status": "incomplete"
}
EOF

# Bound every individual generated/downloaded file. Aggregate bytes and entries are checked
# after each externally executing stage; these are storage bounds, not network-byte claims.
ulimit -f "$max_single_file_blocks"
check_output_bounds() {
  python3 - "$output_root" "$max_output_bytes" "$max_output_entries" <<'PY'
import os, pathlib, stat, sys
root = pathlib.Path(sys.argv[1])
max_bytes, max_entries = map(int, sys.argv[2:])
total = entries = 0
for base, dirs, files in os.walk(root, followlinks=False):
    for name in dirs + files:
        path = pathlib.Path(base, name)
        mode = path.lstat().st_mode
        if stat.S_ISLNK(mode) or not (stat.S_ISDIR(mode) or stat.S_ISREG(mode)):
            raise SystemExit("private output contains an unsupported entry")
        entries += 1
        if entries > max_entries:
            raise SystemExit("private output entry bound exceeded")
        if stat.S_ISREG(mode):
            total += path.stat().st_size
            if total > max_bytes:
                raise SystemExit("private output byte bound exceeded")
PY
}

ndk_dir=${COVALENT_ANDROID_NDK_HOME:-${ANDROID_NDK_HOME:-}}
test -n "$ndk_dir" || fail "set COVALENT_ANDROID_NDK_HOME to Android NDK $expected_ndk"
test -f "$ndk_dir/source.properties" || fail "NDK source.properties is missing"
actual_ndk=$(awk -F= '
  $1 ~ /^Pkg\.Revision[[:space:]]*$/ {
    value = $2
    gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
    print value
    exit
  }
' "$ndk_dir/source.properties")
test "$actual_ndk" = "$expected_ndk" || fail "Android NDK revision differs"
prebuilt_root=$ndk_dir/toolchains/llvm/prebuilt
toolchain=
for candidate in "$prebuilt_root"/*; do
  test -d "$candidate/bin" || continue
  test -z "$toolchain" || fail "more than one NDK host toolchain was found"
  toolchain=$candidate
done
test -n "$toolchain" || fail "NDK host toolchain is missing"

export GOPATH=$cache/gopath
export GOMODCACHE=$cache/gomodcache
export GOCACHE=$cache/gocache
export GOBIN=$bindir
export GOTOOLCHAIN=local
export GOPROXY=https://proxy.golang.org
export GOSUMDB=sum.golang.org
export GOFLAGS=-mod=readonly
target_goflags='-mod=readonly -tags=noupgrade'
mkdir "$GOPATH" "$GOMODCACHE" "$GOCACHE"

host_os=$("$go_bin" env GOHOSTOS)
host_arch=$("$go_bin" env GOHOSTARCH)
if ! "$timeout_bin" "$command_timeout_seconds" env \
    GOOS="$host_os" GOARCH="$host_arch" CGO_ENABLED=0 \
    "$go_bin" install "$govuln_module@$govuln_version" \
    > "$reports/govulncheck-install.stdout" 2> "$reports/govulncheck-install.stderr"; then
  fail "host-native govulncheck provisioning was incomplete"
fi
check_output_bounds
govuln=$bindir/govulncheck
test -x "$govuln" || fail "private govulncheck binary is missing"
govuln_output=$("$govuln" -version 2>&1 || true)
case "$govuln_output" in *"govulncheck@$govuln_version"*) ;; *) fail "govulncheck version differs" ;; esac
ziphash=$GOMODCACHE/cache/download/golang.org/x/vuln/@v/$govuln_version.ziphash
test -f "$ziphash" || fail "govulncheck module zip checksum is missing"
test "$(cat "$ziphash")" = "$govuln_zip_hash" || fail "govulncheck module zip checksum differs"
python3 - "$reports/govulncheck-tool.json" "$govuln_module" "$govuln_version" \
  "$govuln_output" "$(shasum -a 256 "$govuln" | awk '{print $1}')" "$govuln_zip_hash" <<'PY'
import json, pathlib, sys
out, module, version, version_output, digest, zip_hash = sys.argv[1:]
pathlib.Path(out).write_text(json.dumps({
    "module": module, "version": version, "versionOutput": version_output,
    "binarySha256": digest, "moduleZipHash": zip_hash,
}, sort_keys=True, indent=2) + "\n")
PY

collect_target() {
  goarch=$1
  abi=$2
  compiler=$3
  cc=$toolchain/bin/$compiler
  test -x "$cc" || fail "target compiler is missing for $abi"
  target=$reports/android-$goarch
  mkdir "$target"

  if ! "$timeout_bin" "$command_timeout_seconds" env \
      GOOS=android GOARCH="$goarch" CGO_ENABLED=1 CC="$cc" GOFLAGS="$target_goflags" \
      "$go_bin" env -json GOOS GOARCH GOHOSTOS GOHOSTARCH CGO_ENABLED CC GOFLAGS GOVERSION \
      > "$target/go-environment.json" 2> "$target/go-environment.stderr"; then
    fail "$abi target environment collection was incomplete"
  fi
  if ! "$timeout_bin" "$command_timeout_seconds" env \
      GOOS=android GOARCH="$goarch" CGO_ENABLED=1 CC="$cc" GOFLAGS="$target_goflags" \
      "$go_bin" -C "$source_dir" list -mod=readonly -deps -json ./cmd/syncthing \
      > "$target/go-target-deps.ndjson" 2> "$target/go-target-deps.stderr"; then
    fail "$abi target dependency inventory was incomplete"
  fi
  check_output_bounds
  if ! "$timeout_bin" "$command_timeout_seconds" env \
      GOOS=android GOARCH="$goarch" CGO_ENABLED=1 CC="$cc" GOFLAGS="$target_goflags" \
      "$govuln" -C "$source_dir" -db "$official_db" -mode source -scan symbol \
      -format json ./cmd/syncthing \
      > "$target/govulncheck.json" 2> "$target/govulncheck.stderr"; then
    fail "$abi official source symbol scan was incomplete"
  fi
  check_output_bounds
}

collect_target arm64 arm64-v8a aarch64-linux-android26-clang
collect_target amd64 x86_64 x86_64-linux-android26-clang

if ! "$timeout_bin" "$command_timeout_seconds" \
    "$go_bin" -C "$source_dir" mod verify \
    > "$reports/go-mod-verify.txt" 2> "$reports/go-mod-verify.stderr"; then
  fail "downloaded module verification was incomplete"
fi
check_output_bounds

# Retain the exact current official records used to justify package-absence dispositions.
python3 - "$reports/official-osv" <<'PY'
import json, pathlib, sys, urllib.request
out = pathlib.Path(sys.argv[1])
ids = ["GO-2026-5932", "GO-2026-6303", "GO-2026-6354", "GO-2026-6355"]
for osv_id in ids:
    url = f"https://vuln.go.dev/ID/{osv_id}.json"
    request = urllib.request.Request(url, headers={"User-Agent": "covalent-target-evidence/1"})
    with urllib.request.urlopen(request, timeout=30) as response:
        if response.status != 200 or response.geturl() != url:
            raise SystemExit("official OSV fetch did not return the exact endpoint")
        raw = response.read(1024 * 1024 + 1)
    if len(raw) > 1024 * 1024:
        raise SystemExit("official OSV record exceeded its byte bound")
    record = json.loads(raw)
    if record.get("id") != osv_id:
        raise SystemExit("official OSV record ID differs")
    (out / f"{osv_id}.json").write_text(
        json.dumps(record, sort_keys=True, indent=2) + "\n")
PY
check_output_bounds

if ! python3 - "$source_dir" "$reports" "$syncthing_commit" "$govuln_version" <<'PY'
import hashlib, json, pathlib, sys

source = pathlib.Path(sys.argv[1])
reports = pathlib.Path(sys.argv[2])
commit = sys.argv[3]
scanner_version = sys.argv[4]
known_packages = {
    "GO-2026-5932": {
        "golang.org/x/crypto/openpgp", "golang.org/x/crypto/openpgp/packet",
        "golang.org/x/crypto/openpgp/armor", "golang.org/x/crypto/openpgp/clearsign",
        "golang.org/x/crypto/openpgp/errors", "golang.org/x/crypto/openpgp/elgamal",
        "golang.org/x/crypto/openpgp/s2k",
    },
    "GO-2026-6303": {"golang.org/x/crypto/ssh"},
    "GO-2026-6354": {"golang.org/x/crypto/ssh"},
    "GO-2026-6355": {"golang.org/x/crypto/ssh"},
}

def decode_stream(path, max_bytes, max_records):
    if not path.is_file() or path.stat().st_size == 0 or path.stat().st_size > max_bytes:
        raise ValueError(f"missing, empty, or oversized report: {path.name}")
    raw = path.read_text()
    decoder, pos, rows = json.JSONDecoder(), 0, []
    while pos < len(raw):
        while pos < len(raw) and raw[pos].isspace(): pos += 1
        if pos == len(raw): break
        row, pos = decoder.raw_decode(raw, pos)
        rows.append(row)
        if len(rows) > max_records: raise ValueError(f"too many records: {path.name}")
    return rows

official = {}
for osv_id in sorted(known_packages):
    official[osv_id] = json.loads((reports / "official-osv" / f"{osv_id}.json").read_text())

errors, target_summaries = [], []
for goarch, abi in (("arm64", "arm64-v8a"), ("amd64", "x86_64")):
    target = reports / f"android-{goarch}"
    try:
        environment_path = target / "go-environment.json"
        if (not environment_path.is_file() or environment_path.stat().st_size == 0 or
                environment_path.stat().st_size > 64 * 1024):
            raise ValueError("missing, empty, or oversized target environment report")
        environment = json.loads(environment_path.read_text())
        compiler = ("aarch64-linux-android26-clang" if goarch == "arm64"
                    else "x86_64-linux-android26-clang")
        expected_environment = {
            "GOOS": "android", "GOARCH": goarch, "CGO_ENABLED": "1",
            "GOFLAGS": "-mod=readonly -tags=noupgrade", "GOVERSION": "go1.26.7",
        }
        if any(environment.get(key) != value for key, value in expected_environment.items()):
            raise ValueError("recorded Go target environment differs")
        if pathlib.Path(environment.get("CC", "")).name != compiler:
            raise ValueError("recorded C compiler does not select the API-26 target")
        deps = decode_stream(target / "go-target-deps.ndjson", 128 * 1024 * 1024, 16384)
        packages = set()
        compact = []
        for row in deps:
            if row.get("Error") or row.get("DepsErrors"):
                raise ValueError("go list reported a package loading error")
            path = row.get("ImportPath")
            if not path: raise ValueError("go list record lacks ImportPath")
            packages.add(path)
            module = row.get("Module") or {}
            compact.append({"importPath": path, "module": module.get("Path"),
                            "moduleVersion": module.get("Version")})
        root_package = "github.com/syncthing/syncthing/cmd/syncthing"
        if root_package not in packages: raise ValueError("target root package is absent")
        compact.sort(key=lambda row: row["importPath"])
        (target / "target-imports.json").write_text(json.dumps({
            "goos": "android", "goarch": goarch, "abi": abi,
            "packageCount": len(compact), "packages": compact,
        }, sort_keys=True, indent=2) + "\n")

        messages = decode_stream(target / "govulncheck.json", 128 * 1024 * 1024, 32768)
        configs = [row["config"] for row in messages if set(row) == {"config"}]
        sboms = [row["SBOM"] for row in messages if set(row) == {"SBOM"}]
        allowed = {"config", "SBOM", "progress", "osv", "finding"}
        if any(len(row) != 1 or next(iter(row)) not in allowed for row in messages):
            raise ValueError("govulncheck stream contains an unknown message shape")
        if len(configs) != 1 or len(sboms) != 1:
            raise ValueError("govulncheck stream lacks one config and one SBOM")
        config = configs[0]
        required_config = {
            "scanner_name": "govulncheck", "scanner_version": scanner_version,
            "db": "https://vuln.go.dev", "go_version": "go1.26.7",
            "scan_level": "symbol", "scan_mode": "source",
        }
        if any(config.get(key) != value for key, value in required_config.items()):
            raise ValueError("govulncheck configuration differs from the required source symbol scan")
        if not config.get("protocol_version") or not config.get("db_last_modified"):
            raise ValueError("govulncheck database metadata is incomplete")

        scan_osv = {}
        for row in messages:
            if "osv" not in row: continue
            record = row["osv"]
            osv_id = record.get("id")
            if not osv_id: raise ValueError("streamed OSV record lacks ID")
            if osv_id in scan_osv and scan_osv[osv_id] != record:
                raise ValueError("conflicting streamed OSV records")
            scan_osv[osv_id] = record
        findings = [row["finding"] for row in messages if "finding" in row]
        by_id = {}
        finding_rows = []
        target_errors = []
        for finding in findings:
            osv_id = finding.get("osv")
            trace = finding.get("trace")
            if not osv_id or not isinstance(trace, list) or not trace:
                target_errors.append("finding lacks an ID or trace")
                continue
            by_id.setdefault(osv_id, []).append(finding)
            called = any(frame.get("function") for frame in trace)
            package_trace = any(frame.get("package") for frame in trace)
            classification = ("called-symbol" if called else
                              "target-package" if package_trace else "module-only")
            disposition = "unresolved"
            if called:
                disposition = "fail-called-symbol"
                target_errors.append(f"{osv_id} has a called-symbol trace")
            elif osv_id not in known_packages:
                target_errors.append(f"{osv_id} is not covered by a reviewed disposition")
            finding_rows.append({"osv": osv_id, "classification": classification,
                                 "disposition": disposition, "trace": trace,
                                 "fixedVersion": finding.get("fixed_version")})

        dispositions = []
        for osv_id, expected_packages in sorted(known_packages.items()):
            record = official[osv_id]
            streamed = scan_osv.get(osv_id)
            imports = []
            affected = record.get("affected") or []
            record_valid = (record.get("id") == osv_id and
                (record.get("database_specific") or {}).get("review_status") == "REVIEWED" and
                len(affected) == 1 and
                (affected[0].get("package") or {}).get("name") == "golang.org/x/crypto" and
                (affected[0].get("package") or {}).get("ecosystem") == "Go")
            if record_valid:
                imports = (affected[0].get("ecosystem_specific") or {}).get("imports") or []
                record_valid = all(not item.get("goos") and not item.get("goarch")
                                   for item in imports)
            affected_packages = {item.get("path") for item in imports}
            record_valid = record_valid and affected_packages == expected_packages
            finding_group = by_id.get(osv_id, [])
            module_only = (len(finding_group) == 1 and
                finding_group[0].get("trace") == [{
                    "module": "golang.org/x/crypto", "version": "v0.54.0"}])
            absent = sorted(expected_packages & packages) == []
            stream_matches_current = streamed == record
            passed = record_valid and module_only and absent and stream_matches_current
            disposition = {
                "osv": osv_id,
                "status": "not-in-android-target-package-graph" if passed else "failed-review",
                "officialRecordReviewedAndExpected": record_valid,
                "scanRecordMatchesCurrentOfficialRecord": stream_matches_current,
                "findingIsExactModuleOnlyXCryptoV0540": module_only,
                "affectedPackages": sorted(expected_packages),
                "affectedPackagesPresent": sorted(expected_packages & packages),
            }
            dispositions.append(disposition)
            if passed:
                for row in finding_rows:
                    if row["osv"] == osv_id:
                        row["disposition"] = "not-in-android-target-package-graph"
            else:
                target_errors.append(f"{osv_id} did not satisfy every package-absence predicate")

        target_summaries.append({
            "goos": "android", "goarch": goarch, "abi": abi,
            "environment": environment,
            "packageCount": len(packages), "scanConfig": config,
            "findingCount": len(findings), "findings": finding_rows,
            "knownDispositions": dispositions,
            "status": "passed" if not target_errors else "failed",
        })
        errors.extend(f"{abi}: {message}" for message in target_errors)
    except Exception as error:
        errors.append(f"{abi}: incomplete evidence: {type(error).__name__}: {error}")

summary = {
    "schemaVersion": 1,
    "status": "passed" if not errors else "failed",
    "source": {
        "version": "v2.1.3", "commit": commit,
        "goModSha256": hashlib.sha256((source / "go.mod").read_bytes()).hexdigest(),
        "goSumSha256": hashlib.sha256((source / "go.sum").read_bytes()).hexdigest(),
    },
    "scanner": {"module": "golang.org/x/vuln/cmd/govulncheck",
                "version": scanner_version, "database": "https://vuln.go.dev"},
    "scope": "source symbol analysis for two Android CGO/API-26 build configurations; not runtime evidence",
    "targets": target_summaries,
    "errors": errors,
}
(reports / "summary.json").write_text(json.dumps(summary, sort_keys=True, indent=2) + "\n")
if errors: raise SystemExit(1)
PY
then
  fail "Android target vulnerability evidence is incomplete or has unresolved findings"
fi

check_output_bounds
test -z "$(git -C "$source_dir" status --porcelain=v1 --untracked-files=all)" ||
  fail "evidence collection modified the pinned source checkout"
cat > "$reports/collection-status.json" <<'EOF'
{
  "schemaVersion": 1,
  "status": "passed"
}
EOF
printf '%s\n' "Android target dependency and vulnerability evidence passed: $reports/summary.json"
