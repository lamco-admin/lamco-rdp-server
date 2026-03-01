#!/usr/bin/env python3
"""Generate DEP-5 debian/copyright from cargo metadata and crates.io API.

Reads vendored crate metadata, resolves authors via cargo metadata and
crates.io API (with local JSON cache), normalizes SPDX expressions, and
emits a grouped DEP-5 copyright file.

Usage:
    python3 packaging/scripts/generate-debian-copyright.py [OPTIONS]
"""

import argparse
import json
import logging
import os
import re
import subprocess
import sys
import tarfile
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field, asdict
from pathlib import Path
from typing import Optional

log = logging.getLogger("dep5")

# ---------------------------------------------------------------------------
# Data model
# ---------------------------------------------------------------------------

@dataclass
class CrateInfo:
    name: str
    version: str
    license: str
    authors: list[str] = field(default_factory=list)
    repository: Optional[str] = None
    source_kind: str = ""       # "crates.io", "git", "bundled"
    resolution: str = "unknown" # "metadata", "cache", "api", "override", "UNRESOLVED"


# ---------------------------------------------------------------------------
# SPDX normalization
# ---------------------------------------------------------------------------

class SPDXNormalizer:
    """Normalize SPDX license expressions to canonical form.

    Cargo metadata uses several spellings for the same thing:
      MIT OR Apache-2.0, Apache-2.0 OR MIT, MIT/Apache-2.0, Apache-2.0/MIT
    All of these mean the same dual license. We canonicalize by splitting on
    OR and /, sorting alphabetically, and rejoining with ' OR '.

    AND expressions are preserved as-is within their components.
    """

    # Licenses Debian ships full text for in /usr/share/common-licenses/
    COMMON_LICENSES = {
        "Apache-2.0", "Artistic-2.0", "BSD-2-Clause", "BSD-3-Clause",
        "CC0-1.0", "GFDL-1.3", "GPL-2.0", "GPL-3.0", "LGPL-2.1",
        "LGPL-3.0", "MPL-2.0",
    }

    @staticmethod
    def normalize(expr: str) -> str:
        if not expr:
            return "UNKNOWN"

        # Parenthesized compound expressions like "(MIT OR Apache-2.0) AND Unicode-3.0"
        # require special handling to avoid mangling the AND grouping.
        # Strategy: split top-level AND first, normalize OR within each part.
        and_parts = SPDXNormalizer._split_top_level(expr, "AND")
        normalized_and = []
        for part in and_parts:
            part = part.strip().strip("()")
            or_parts = SPDXNormalizer._split_top_level(part, "OR")
            if len(or_parts) == 1:
                # No OR — might be a slash-separated list
                or_parts = [p.strip() for p in part.split("/") if p.strip()]
            else:
                or_parts = [p.strip().strip("()") for p in or_parts]
            or_parts.sort(key=str.casefold)
            normalized_and.append(" OR ".join(or_parts))

        normalized_and.sort(key=str.casefold)
        result = " AND ".join(normalized_and)

        # When AND joins multiple parts and any part contains OR,
        # wrap those parts in parens for unambiguous SPDX
        if len(normalized_and) > 1:
            wrapped = []
            for part in normalized_and:
                if " OR " in part:
                    wrapped.append(f"({part})")
                else:
                    wrapped.append(part)
            wrapped.sort(key=str.casefold)
            result = " AND ".join(wrapped)

        return result

    @staticmethod
    def _split_top_level(expr: str, keyword: str) -> list[str]:
        """Split expression on a keyword (AND/OR) respecting parentheses."""
        parts = []
        depth = 0
        current = []
        # Pad with spaces for word-boundary matching
        tokens = expr.replace("(", " ( ").replace(")", " ) ").split()
        for token in tokens:
            if token == "(":
                depth += 1
                current.append(token)
            elif token == ")":
                depth -= 1
                current.append(token)
            elif token.upper() == keyword and depth == 0:
                parts.append(" ".join(current))
                current = []
            else:
                current.append(token)
        if current:
            parts.append(" ".join(current))
        return parts if parts else [expr]

    @staticmethod
    def has_common_license(license_id: str) -> bool:
        return license_id in SPDXNormalizer.COMMON_LICENSES


# ---------------------------------------------------------------------------
# Cache
# ---------------------------------------------------------------------------

class CopyrightCache:
    """JSON file cache keyed by (name, version) tuples."""

    def __init__(self, path: Path):
        self.path = path
        self.data: dict[str, dict] = {}
        self.dirty = False

    def load(self):
        if self.path.exists():
            with open(self.path) as f:
                self.data = json.load(f)
            log.info("Loaded cache with %d entries", len(self.data))
        else:
            log.info("No cache file found, starting fresh")

    def save(self):
        if not self.dirty:
            return
        self.path.parent.mkdir(parents=True, exist_ok=True)
        with open(self.path, "w") as f:
            json.dump(self.data, f, indent=2, sort_keys=True)
            f.write("\n")
        log.info("Saved cache with %d entries", len(self.data))

    def _key(self, name: str, version: str) -> str:
        return f"{name}@{version}"

    def get(self, name: str, version: str) -> Optional[list[str]]:
        entry = self.data.get(self._key(name, version))
        if entry:
            return entry.get("authors")
        return None

    def put(self, name: str, version: str, authors: list[str]):
        self.data[self._key(name, version)] = {
            "authors": authors,
            "fetched": time.strftime("%Y-%m-%d"),
        }
        self.dirty = True


# ---------------------------------------------------------------------------
# crates.io API client
# ---------------------------------------------------------------------------

class CratesIOClient:
    """Rate-limited crates.io API client using only urllib."""

    BASE = "https://crates.io/api/v1"
    USER_AGENT = "lamco-copyright-tool (greg@lamco.io)"

    def __init__(self):
        self.last_request = 0.0

    def _rate_limit(self):
        elapsed = time.monotonic() - self.last_request
        if elapsed < 1.0:
            time.sleep(1.0 - elapsed)
        self.last_request = time.monotonic()

    def _get(self, path: str) -> Optional[dict]:
        url = f"{self.BASE}{path}"
        req = urllib.request.Request(url)
        req.add_header("User-Agent", self.USER_AGENT)
        self._rate_limit()
        try:
            with urllib.request.urlopen(req, timeout=15) as resp:
                return json.loads(resp.read())
        except urllib.error.HTTPError as e:
            log.warning("API %s returned %d", url, e.code)
            return None
        except (urllib.error.URLError, OSError) as e:
            log.warning("API %s failed: %s", url, e)
            return None

    def get_version_info(self, name: str, version: str) -> Optional[dict]:
        return self._get(f"/crates/{name}/{version}")

    def get_owners(self, name: str) -> Optional[list[dict]]:
        data = self._get(f"/crates/{name}/owners")
        if data:
            return data.get("users", [])
        return None


# ---------------------------------------------------------------------------
# Cargo metadata extraction
# ---------------------------------------------------------------------------

def run_cargo_metadata(project_dir: Path) -> dict:
    """Run cargo metadata and return parsed JSON.

    Uses --all-features so optional dependencies (iced/gui, rfd) are included
    in the dependency graph. The Debian package builds with all features enabled.
    """
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--all-features"],
        cwd=project_dir,
        capture_output=True,
        text=True,
        timeout=120,
    )
    if result.returncode != 0:
        log.error("cargo metadata failed:\n%s", result.stderr)
        sys.exit(1)
    return json.loads(result.stdout)


def extract_external_deps(meta: dict, project_dir: Path) -> tuple[list[CrateInfo], list[CrateInfo]]:
    """Extract external and bundled dependencies from cargo metadata.

    Returns (external_crates, bundled_crates).
    Excludes the workspace root package and dev-only dependencies.
    """
    workspace_members = set(meta.get("workspace_members", []))
    resolve = meta.get("resolve", {})
    root_id = resolve.get("root", "")

    # Build set of package IDs reachable from root via non-dev deps
    id_to_node = {}
    for node in resolve.get("nodes", []):
        id_to_node[node["id"]] = node

    reachable = set()
    queue = [root_id]
    while queue:
        pid = queue.pop()
        if pid in reachable:
            continue
        reachable.add(pid)
        node = id_to_node.get(pid)
        if not node:
            continue
        for dep in node.get("deps", []):
            # Skip dev and build deps
            dep_kinds = {k.get("kind") for k in dep.get("dep_kinds", [])}
            if dep_kinds <= {"dev", "build"}:
                continue
            queue.append(dep["pkg"])

    # Map package ID to package data
    id_to_pkg = {}
    for p in meta["packages"]:
        id_to_pkg[p["id"]] = p

    external = []
    bundled = []
    bundled_dir = str(project_dir / "bundled-crates")

    for pid in sorted(reachable):
        if pid in workspace_members:
            continue
        pkg = id_to_pkg.get(pid)
        if not pkg:
            continue

        source = pkg.get("source")
        manifest = pkg.get("manifest_path", "")

        # Bundled path deps
        if source is None and bundled_dir in manifest:
            bundled.append(CrateInfo(
                name=pkg["name"],
                version=pkg["version"],
                license=pkg.get("license", ""),
                authors=pkg.get("authors", []),
                repository=pkg.get("repository"),
                source_kind="bundled",
                resolution="metadata" if pkg.get("authors") else "unknown",
            ))
            continue

        # Skip other path deps (workspace root)
        if source is None:
            continue

        kind = "git" if "git" in (source or "") else "crates.io"
        info = CrateInfo(
            name=pkg["name"],
            version=pkg["version"],
            license=pkg.get("license", ""),
            authors=pkg.get("authors", []),
            repository=pkg.get("repository"),
            source_kind=kind,
            resolution="metadata" if pkg.get("authors") else "unknown",
        )
        external.append(info)

    return external, bundled


def scan_vendor_for_missing(
    known_crates: list[CrateInfo],
    vendor_dir: Optional[Path] = None,
    tarball: Optional[Path] = None,
) -> list[CrateInfo]:
    """Scan vendor/ for crates not already in the known list.

    cargo metadata's resolve graph only includes runtime deps. But the vendor
    tarball also contains dev-deps and build-deps (cc, cmake, criterion, etc.)
    that are physically shipped in the source. DEP-5 must cover all shipped files.

    Reads from either a local vendor/ directory or a tarball.
    """
    try:
        import tomllib
    except ImportError:
        import tomli as tomllib

    known = {(c.name, c.version) for c in known_crates}
    extra = []

    def _process_toml(raw: bytes, fallback_name: str):
        data = tomllib.loads(raw.decode("utf-8"))
        pkg = data.get("package", {})
        name = pkg.get("name", fallback_name)
        version = pkg.get("version", "0.0.0")
        if isinstance(version, dict):
            version = version.get("workspace", "0.0.0")
        if (name, version) in known:
            return
        license_expr = pkg.get("license", "")
        if isinstance(license_expr, dict):
            license_expr = license_expr.get("workspace", "")
        authors = pkg.get("authors", [])
        extra.append(CrateInfo(
            name=name,
            version=version,
            license=license_expr,
            authors=authors,
            repository=pkg.get("repository"),
            source_kind="crates.io",
            resolution="metadata" if authors else "unknown",
        ))

    if tarball and tarball.exists():
        cargo_toml_re = re.compile(r'^[^/]+/vendor/([^/]+)/Cargo\.toml$')
        with tarfile.open(tarball, 'r:*') as tf:
            for member in tf.getmembers():
                m = cargo_toml_re.match(member.name)
                if not m:
                    continue
                f = tf.extractfile(member)
                if f is None:
                    continue
                _process_toml(f.read(), m.group(1))
    elif vendor_dir and vendor_dir.is_dir():
        for cargo_toml in sorted(vendor_dir.glob("*/Cargo.toml")):
            _process_toml(cargo_toml.read_bytes(), cargo_toml.parent.name)

    return extra


# ---------------------------------------------------------------------------
# Author resolution
# ---------------------------------------------------------------------------

def clean_author(author: str) -> str:
    """Normalize author string: strip email noise, deduplicate."""
    author = author.strip()
    # Remove trailing email-only entries like "<foo@bar>"
    if author.startswith("<") and author.endswith(">"):
        return author[1:-1]
    return author


def resolve_authors(
    crates: list[CrateInfo],
    cache: CopyrightCache,
    client: Optional[CratesIOClient],
    overrides: dict,
    no_api: bool = False,
) -> list[str]:
    """Resolve authors for all crates. Returns list of unresolved crate names."""
    unresolved = []
    api_calls = 0

    for crate in crates:
        key = f"{crate.name}@{crate.version}"

        # 1. Manual overrides (highest priority)
        if key in overrides or crate.name in overrides:
            override = overrides.get(key, overrides.get(crate.name, {}))
            if "authors" in override:
                crate.authors = override["authors"]
                crate.resolution = "override"
                log.debug("  %s: override -> %s", key, crate.authors)
                continue

        # 2. Cache hit
        cached = cache.get(crate.name, crate.version)
        if cached:
            crate.authors = cached
            crate.resolution = "cache"
            log.debug("  %s: cache -> %s", key, crate.authors)
            continue

        # 3. Cargo metadata (already populated if available)
        if crate.authors:
            crate.resolution = "metadata"
            cache.put(crate.name, crate.version, crate.authors)
            log.debug("  %s: metadata -> %s", key, crate.authors)
            continue

        # 4-5. crates.io API fallback
        if no_api or client is None:
            unresolved.append(crate.name)
            crate.resolution = "UNRESOLVED"
            log.debug("  %s: UNRESOLVED (API disabled)", key)
            continue

        # Try version endpoint for published_by
        resolved = False
        version_data = client.get_version_info(crate.name, crate.version)
        api_calls += 1
        if version_data:
            version_info = version_data.get("version", {})
            published_by = version_info.get("published_by")
            if published_by:
                name = published_by.get("name") or published_by.get("login", "")
                if name:
                    crate.authors = [name]
                    crate.resolution = "api"
                    cache.put(crate.name, crate.version, crate.authors)
                    log.debug("  %s: api/published_by -> %s", key, crate.authors)
                    resolved = True

        if not resolved:
            # Try owners endpoint
            owners = client.get_owners(crate.name)
            api_calls += 1
            if owners:
                names = [o.get("name") or o.get("login", "") for o in owners if o.get("kind") == "user"]
                names = [n for n in names if n]
                if names:
                    crate.authors = names
                    crate.resolution = "api"
                    cache.put(crate.name, crate.version, crate.authors)
                    log.debug("  %s: api/owners -> %s", key, crate.authors)
                    resolved = True

        if not resolved:
            unresolved.append(key)
            crate.resolution = "UNRESOLVED"
            log.warning("  %s: UNRESOLVED after API lookup", key)

    log.info("Author resolution: %d API calls, %d unresolved", api_calls, len(unresolved))
    return unresolved


# ---------------------------------------------------------------------------
# License grouping
# ---------------------------------------------------------------------------

def group_by_license(crates: list[CrateInfo]) -> dict[str, list[CrateInfo]]:
    """Group crates by normalized SPDX license expression."""
    groups: dict[str, list[CrateInfo]] = {}
    for crate in crates:
        key = SPDXNormalizer.normalize(crate.license)
        groups.setdefault(key, []).append(crate)
    # Sort crates within each group by name
    for group in groups.values():
        group.sort(key=lambda c: c.name)
    return groups


# ---------------------------------------------------------------------------
# DEP-5 writer
# ---------------------------------------------------------------------------

class DEP5Writer:
    """Assemble a DEP-5 format copyright file."""

    def __init__(self, project_dir: Path):
        self.project_dir = project_dir
        self.lines: list[str] = []

    def _add(self, text: str = ""):
        self.lines.append(text)

    def _wrap_license_text(self, text: str) -> str:
        """Format license body text for DEP-5: each line prefixed with space,
        blank lines become ' .'"""
        out = []
        for line in text.splitlines():
            stripped = line.rstrip()
            if not stripped:
                out.append(" .")
            else:
                out.append(f" {stripped}")
        return "\n".join(out)

    def write_header(self):
        self._add("Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/")
        self._add("Upstream-Name: lamco-rdp-server")
        self._add("Upstream-Contact: Greg Lamberson <greg@lamco.io>")
        self._add("Source: https://github.com/lamco-admin/lamco-rdp-server")

    def write_main_project(self):
        self._add("")
        self._add("Files: *")
        self._add("Copyright: 2025-2026 Lamco Development")
        self._add("License: BUSL-1.1")
        # AppStream metainfo is CC0-1.0 per convention
        metainfo = self.project_dir / "data" / "io.lamco.rdp-server.metainfo.xml"
        if metainfo.exists():
            self._add("")
            self._add("Files: data/io.lamco.rdp-server.metainfo.xml")
            self._add("Copyright: 2025-2026 Lamco Development")
            self._add("License: CC0-1.0")

    def write_bundled_crates(self, bundled: list[CrateInfo]):
        if not bundled:
            return
        self._add("")
        files = " ".join(f"bundled-crates/{c.name}/*" for c in sorted(bundled, key=lambda c: c.name))
        self._add(f"Files: {files}")
        authors = set()
        for c in bundled:
            for a in c.authors:
                authors.add(clean_author(a))
        if not authors:
            authors = {"Lamco Development"}
        copyright_lines = sorted(authors)
        self._add(f"Copyright: {copyright_lines[0]}")
        for a in copyright_lines[1:]:
            self._add(f"           {a}")
        # Use the license from the bundled crates
        licenses = set(SPDXNormalizer.normalize(c.license) for c in bundled)
        self._add(f"License: {' AND '.join(sorted(licenses))}")

    def write_vendor_groups(self, groups: dict[str, list[CrateInfo]], multi_version_crates: set[str] = None):
        """Write one Files stanza per unique license group."""
        if multi_version_crates is None:
            multi_version_crates = set()

        for license_expr in sorted(groups.keys()):
            crates = groups[license_expr]
            self._add("")

            # Determine actual vendor directory name: cargo vendor only
            # appends version when multiple versions of the same crate coexist.
            # Check the filesystem first, then fall back to heuristic.
            file_patterns = []
            for c in crates:
                versioned = self.project_dir / "vendor" / f"{c.name}-{c.version}"
                unversioned = self.project_dir / "vendor" / c.name
                if versioned.is_dir():
                    file_patterns.append(f"vendor/{c.name}-{c.version}/*")
                elif unversioned.is_dir():
                    file_patterns.append(f"vendor/{c.name}/*")
                elif c.name in multi_version_crates:
                    # Multiple versions exist: cargo vendor uses versioned dirs
                    file_patterns.append(f"vendor/{c.name}-{c.version}/*")
                else:
                    # Single version: cargo vendor uses unversioned dir
                    file_patterns.append(f"vendor/{c.name}/*")
            files_str = " ".join(file_patterns)
            self._add(f"Files: {files_str}")

            # Aggregate all unique authors
            all_authors: set[str] = set()
            for c in crates:
                for a in c.authors:
                    cleaned = clean_author(a)
                    if cleaned:
                        all_authors.add(cleaned)

            if not all_authors:
                all_authors = {"See individual crate files"}

            copyright_lines = sorted(all_authors)
            self._add(f"Copyright: {copyright_lines[0]}")
            for a in copyright_lines[1:]:
                self._add(f"           {a}")
            self._add(f"License: {license_expr}")

    def write_license_texts(self, groups: dict[str, list[CrateInfo]], bundled: list[CrateInfo]):
        """Write License: blocks with full text or Debian common-license refs."""
        all_licenses: set[str] = set()
        all_licenses.add("BUSL-1.1")
        for expr in groups:
            for part in re.split(r'\s+(?:OR|AND)\s+', expr):
                part = part.strip().strip("()")
                if part:
                    all_licenses.add(part)
        for c in bundled:
            for part in re.split(r'\s+(?:OR|AND)\s+', SPDXNormalizer.normalize(c.license)):
                part = part.strip().strip("()")
                if part:
                    all_licenses.add(part)

        for lic in sorted(all_licenses):
            self._add("")
            self._add(f"License: {lic}")
            if lic == "BUSL-1.1":
                self._write_busl_text()
            elif SPDXNormalizer.has_common_license(lic):
                self._add(f" On Debian systems, the full text of the {lic} license")
                self._add(f" can be found in /usr/share/common-licenses/{lic}.")
            elif lic == "MIT":
                self._write_mit_text()
            elif lic == "ISC":
                self._write_isc_text()
            elif lic == "Unlicense":
                self._write_unlicense_text()
            elif lic == "Unicode-3.0":
                self._write_unicode_text()
            elif lic == "Zlib":
                self._write_zlib_text()
            elif lic == "0BSD":
                self._write_0bsd_text()
            elif lic == "MIT-0":
                self._write_mit0_text()
            elif lic == "BSL-1.0":
                self._write_bsl1_text()
            elif lic.startswith("Apache-2.0 WITH"):
                self._add(f" On Debian systems, the Apache-2.0 base license can be found in")
                self._add(f" /usr/share/common-licenses/Apache-2.0.")
                self._add(f" .")
                self._add(f" This usage includes the LLVM exception which grants additional")
                self._add(f" permissions for code compiled with LLVM.")
            elif lic == "OpenSSL":
                self._write_openssl_text()
            else:
                self._add(f" See the individual crate directories for the full license text.")

    def _write_busl_text(self):
        """Read BUSL-1.1 text verbatim from the project's LICENSE file."""
        license_path = self.project_dir / "LICENSE"
        if not license_path.exists():
            self._add(" See the LICENSE file in the source distribution.")
            log.warning("LICENSE file not found at %s", license_path)
            return
        text = license_path.read_text()
        self._add(self._wrap_license_text(text))

    def _write_mit_text(self):
        self._add(" Permission is hereby granted, free of charge, to any person obtaining a")
        self._add(" copy of this software and associated documentation files (the \"Software\"),")
        self._add(" to deal in the Software without restriction, including without limitation")
        self._add(" the rights to use, copy, modify, merge, publish, distribute, sublicense,")
        self._add(" and/or sell copies of the Software, and to permit persons to whom the")
        self._add(" Software is furnished to do so, subject to the following conditions:")
        self._add(" .")
        self._add(" The above copyright notice and this permission notice shall be included")
        self._add(" in all copies or substantial portions of the Software.")
        self._add(" .")
        self._add(" THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS")
        self._add(" OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,")
        self._add(" FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL")
        self._add(" THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER")
        self._add(" LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING")
        self._add(" FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER")
        self._add(" DEALINGS IN THE SOFTWARE.")

    def _write_isc_text(self):
        self._add(" Permission to use, copy, modify, and/or distribute this software for any")
        self._add(" purpose with or without fee is hereby granted, provided that the above")
        self._add(" copyright notice and this permission notice appear in all copies.")
        self._add(" .")
        self._add(" THE SOFTWARE IS PROVIDED \"AS IS\" AND THE AUTHOR DISCLAIMS ALL WARRANTIES")
        self._add(" WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF")
        self._add(" MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR")
        self._add(" ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES")
        self._add(" WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN")
        self._add(" ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF")
        self._add(" OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.")

    def _write_unlicense_text(self):
        self._add(" This is free and unencumbered software released into the public domain.")
        self._add(" .")
        self._add(" Anyone is free to copy, modify, publish, use, compile, sell, or distribute")
        self._add(" this software, either in source code form or as a compiled binary, for any")
        self._add(" purpose, commercial or non-commercial, and by any means.")
        self._add(" .")
        self._add(" In jurisdictions that recognize copyright laws, the author or authors of")
        self._add(" this software dedicate any and all copyright interest in the software to")
        self._add(" the public domain.")
        self._add(" .")
        self._add(" THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS")
        self._add(" OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,")
        self._add(" FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL")
        self._add(" THE AUTHORS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER")
        self._add(" IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN")
        self._add(" CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.")

    def _write_unicode_text(self):
        self._add(" UNICODE LICENSE V3")
        self._add(" .")
        self._add(" Permission is hereby granted, free of charge, to any person obtaining a")
        self._add(" copy of data files and any associated documentation (the \"Data Files\") or")
        self._add(" software and any associated documentation (the \"Software\") to deal in the")
        self._add(" Data Files or Software without restriction, including without limitation")
        self._add(" the rights to use, copy, modify, merge, publish, distribute, and/or sell")
        self._add(" copies of the Data Files or Software, and to permit persons to whom the")
        self._add(" Data Files or Software are furnished to do so, provided that either (a)")
        self._add(" this copyright and permission notice appear with all copies of the Data")
        self._add(" Files or Software, or (b) this copyright and permission notice appear in")
        self._add(" associated Documentation.")
        self._add(" .")
        self._add(" THE DATA FILES AND SOFTWARE ARE PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY")
        self._add(" KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF")
        self._add(" MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT OF")
        self._add(" THIRD PARTY RIGHTS.")

    def _write_zlib_text(self):
        self._add(" This software is provided 'as-is', without any express or implied")
        self._add(" warranty. In no event will the authors be held liable for any damages")
        self._add(" arising from the use of this software.")
        self._add(" .")
        self._add(" Permission is granted to anyone to use this software for any purpose,")
        self._add(" including commercial applications, and to alter it and redistribute it")
        self._add(" freely, subject to the following restrictions:")
        self._add(" .")
        self._add(" 1. The origin of this software must not be misrepresented; you must not")
        self._add("    claim that you wrote the original software. If you use this software in")
        self._add("    a product, an acknowledgment in the product documentation would be")
        self._add("    appreciated but is not required.")
        self._add(" 2. Altered source versions must be plainly marked as such, and must not be")
        self._add("    misrepresented as being the original software.")
        self._add(" 3. This notice may not be removed or altered from any source distribution.")

    def _write_0bsd_text(self):
        self._add(" Permission to use, copy, modify, and/or distribute this software for any")
        self._add(" purpose with or without fee is hereby granted.")
        self._add(" .")
        self._add(" THE SOFTWARE IS PROVIDED \"AS IS\" AND THE AUTHOR DISCLAIMS ALL WARRANTIES")
        self._add(" WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF")
        self._add(" MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR")
        self._add(" ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES")
        self._add(" WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN")
        self._add(" ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF")
        self._add(" OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.")

    def _write_mit0_text(self):
        self._add(" MIT No Attribution")
        self._add(" .")
        self._add(" Permission is hereby granted, free of charge, to any person obtaining a")
        self._add(" copy of this software and associated documentation files (the \"Software\"),")
        self._add(" to deal in the Software without restriction, including without limitation")
        self._add(" the rights to use, copy, modify, merge, publish, distribute, sublicense,")
        self._add(" and/or sell copies of the Software, and to permit persons to whom the")
        self._add(" Software is furnished to do so.")
        self._add(" .")
        self._add(" THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS")
        self._add(" OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,")
        self._add(" FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL")
        self._add(" THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER")
        self._add(" LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING")
        self._add(" FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER")
        self._add(" DEALINGS IN THE SOFTWARE.")

    def _write_bsl1_text(self):
        self._add(" Boost Software License - Version 1.0 - August 17th, 2003")
        self._add(" .")
        self._add(" Permission is hereby granted, free of charge, to any person or")
        self._add(" organization obtaining a copy of the software and accompanying")
        self._add(" documentation covered by this license (the \"Software\") to use, reproduce,")
        self._add(" display, distribute, execute, and transmit the Software, and to prepare")
        self._add(" derivative works of the Software, and to permit third-parties to whom the")
        self._add(" Software is furnished to do so, all subject to the following:")
        self._add(" .")
        self._add(" The copyright notices in the Software and this entire statement, including")
        self._add(" the above license grant, this restriction and the following disclaimer,")
        self._add(" must be included in all copies of the Software, in whole or in part, and")
        self._add(" all derivative works of the Software, unless such copies or derivative")
        self._add(" works are solely in the form of machine-executable object code generated")
        self._add(" by a source language processor.")
        self._add(" .")
        self._add(" THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS")
        self._add(" OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,")
        self._add(" FITNESS FOR A PARTICULAR PURPOSE, TITLE AND NON-INFRINGEMENT. IN NO EVENT")
        self._add(" SHALL THE COPYRIGHT HOLDERS OR ANYONE DISTRIBUTING THE SOFTWARE BE LIABLE")
        self._add(" FOR ANY DAMAGES OR OTHER LIABILITY, WHETHER IN CONTRACT, TORT OR OTHERWISE,")
        self._add(" ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR")
        self._add(" OTHER DEALINGS IN THE SOFTWARE.")

    def _write_openssl_text(self):
        self._add(" OpenSSL License")
        self._add(" .")
        self._add(" Redistribution and use in source and binary forms, with or without")
        self._add(" modification, are permitted provided that the following conditions are met:")
        self._add(" .")
        self._add(" See https://www.openssl.org/source/license.html for the full license text.")

    def build(self, groups: dict[str, list[CrateInfo]], bundled: list[CrateInfo]) -> str:
        self.lines = []
        self.write_header()
        self.write_main_project()
        self.write_bundled_crates(bundled)

        # Detect crates with multiple versions (cargo vendor appends version
        # to directory name only when multiple versions coexist)
        from collections import Counter
        all_crates = [c for group in groups.values() for c in group]
        name_counts = Counter(c.name for c in all_crates)
        multi_version = {name for name, count in name_counts.items() if count > 1}

        self.write_vendor_groups(groups, multi_version)
        self.write_license_texts(groups, bundled)
        return "\n".join(self.lines) + "\n"


# ---------------------------------------------------------------------------
# Report
# ---------------------------------------------------------------------------

def print_report(
    external: list[CrateInfo],
    bundled: list[CrateInfo],
    groups: dict[str, list[CrateInfo]],
    unresolved: list[str],
):
    """Print a summary report to stderr."""
    total = len(external) + len(bundled)
    print(f"\n{'='*60}", file=sys.stderr)
    print(f"DEP-5 Copyright Generation Report", file=sys.stderr)
    print(f"{'='*60}", file=sys.stderr)
    print(f"Total dependencies:  {total}", file=sys.stderr)
    print(f"  External (vendor): {len(external)}", file=sys.stderr)
    print(f"  Bundled:           {len(bundled)}", file=sys.stderr)
    print(f"License groups:      {len(groups)}", file=sys.stderr)
    print(f"Stanzas generated:   {len(groups) + 2} (header + main + bundled + {len(groups)} vendor groups)", file=sys.stderr)

    # Resolution stats
    res_counts = {}
    for c in external:
        res_counts[c.resolution] = res_counts.get(c.resolution, 0) + 1
    print(f"\nAuthor resolution:", file=sys.stderr)
    for method in ["metadata", "cache", "api", "override", "UNRESOLVED"]:
        if method in res_counts:
            print(f"  {method:12s}: {res_counts[method]}", file=sys.stderr)

    if unresolved:
        print(f"\nUnresolved crates ({len(unresolved)}):", file=sys.stderr)
        for name in sorted(unresolved):
            print(f"  - {name}", file=sys.stderr)

    print(f"{'='*60}\n", file=sys.stderr)


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(
        description="Generate DEP-5 debian/copyright from cargo metadata"
    )
    parser.add_argument(
        "--output", type=Path,
        default=Path("packaging/debian/copyright"),
        help="Output file (default: packaging/debian/copyright)",
    )
    parser.add_argument(
        "--cache", type=Path,
        default=Path("packaging/scripts/copyright-cache.json"),
        help="Cache file (default: packaging/scripts/copyright-cache.json)",
    )
    parser.add_argument(
        "--overrides", type=Path,
        default=Path("packaging/scripts/copyright-overrides.json"),
        help="Overrides file (default: packaging/scripts/copyright-overrides.json)",
    )
    parser.add_argument(
        "--tarball", type=Path, default=None,
        help="Source tarball to scan vendor/ from (supplements cargo metadata)",
    )
    parser.add_argument(
        "--no-cache", action="store_true",
        help="Skip cache, re-resolve everything",
    )
    parser.add_argument(
        "--no-api", action="store_true",
        help="Skip crates.io API calls, use only cargo metadata + cache",
    )
    parser.add_argument(
        "--dry-run", action="store_true",
        help="Print to stdout instead of writing file",
    )
    parser.add_argument(
        "--verbose", action="store_true",
        help="Debug logging",
    )
    parser.add_argument(
        "--project-dir", type=Path, default=None,
        help="Project root (default: auto-detect from script location)",
    )
    args = parser.parse_args()

    logging.basicConfig(
        level=logging.DEBUG if args.verbose else logging.INFO,
        format="%(levelname)s: %(message)s",
    )

    # Resolve project directory
    if args.project_dir:
        project_dir = args.project_dir.resolve()
    else:
        # Walk up from script location to find Cargo.toml
        script_dir = Path(__file__).resolve().parent
        project_dir = script_dir
        while project_dir != project_dir.parent:
            if (project_dir / "Cargo.toml").exists():
                break
            project_dir = project_dir.parent
        else:
            log.error("Could not find Cargo.toml in parent directories")
            sys.exit(1)

    log.info("Project directory: %s", project_dir)

    # Make relative paths absolute from project dir
    for attr in ("output", "cache", "overrides"):
        path = getattr(args, attr)
        if not path.is_absolute():
            setattr(args, attr, project_dir / path)

    # Load overrides
    overrides = {}
    if args.overrides.exists():
        with open(args.overrides) as f:
            overrides = json.load(f)
        log.info("Loaded %d overrides", len(overrides))

    # Load cache
    cache = CopyrightCache(args.cache)
    if not args.no_cache:
        cache.load()

    # Run cargo metadata
    log.info("Running cargo metadata...")
    meta = run_cargo_metadata(project_dir)

    # Extract dependencies
    log.info("Extracting dependencies...")
    external, bundled = extract_external_deps(meta, project_dir)
    log.info("Found %d external deps, %d bundled deps", len(external), len(bundled))

    # Scan vendor/ for crates missed by cargo metadata (dev/build deps)
    vendor_dir = project_dir / "vendor"
    tarball_path = getattr(args, 'tarball', None)
    if tarball_path and not tarball_path.is_absolute():
        tarball_path = project_dir / tarball_path
    vendor_extra = scan_vendor_for_missing(external + bundled, vendor_dir=vendor_dir, tarball=tarball_path)
    if vendor_extra:
        log.info("Found %d additional crates in vendor/ (dev/build deps)", len(vendor_extra))
        external.extend(vendor_extra)

    # Resolve authors
    log.info("Resolving authors...")
    client = None if args.no_api else CratesIOClient()
    unresolved = resolve_authors(external, cache, client, overrides, args.no_api)

    # Save cache
    cache.save()

    # Group by license
    groups = group_by_license(external)
    log.info("Grouped into %d license categories", len(groups))

    # Generate DEP-5
    writer = DEP5Writer(project_dir)
    output = writer.build(groups, bundled)

    # Print report
    print_report(external, bundled, groups, unresolved)

    # Write or print
    if args.dry_run:
        sys.stdout.write(output)
    else:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        with open(args.output, "w") as f:
            f.write(output)
        log.info("Wrote %s (%d bytes)", args.output, len(output))


if __name__ == "__main__":
    main()
