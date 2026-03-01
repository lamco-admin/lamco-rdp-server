#!/usr/bin/env python3
"""Generate RPM spec Provides: bundled(crate(...)) lines and License tag.

Reads vendor/*/Cargo.toml from either a source tree or a tarball to produce:
  1. Sorted Provides: bundled(crate(...)) = VERSION lines for the spec
  2. Composite SPDX License: tag covering all vendored crates + the project

Usage:
    # From extracted source tree (vendor/ directory must exist):
    python3 packaging/scripts/generate-rpm-provides.py

    # From tarball:
    python3 packaging/scripts/generate-rpm-provides.py --tarball path/to/pkg.tar.xz

    # Write directly into spec file:
    python3 packaging/scripts/generate-rpm-provides.py --update-spec packaging/rpmfusion/lamco-rdp-server.spec
"""

import argparse
import json
import re
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

try:
    import tomllib
except ImportError:
    import tomli as tomllib  # Python < 3.11


# ---------------------------------------------------------------------------
# SPDX helpers
# ---------------------------------------------------------------------------

# Cargo license field uses "/" as shorthand for OR
SLASH_RE = re.compile(r'\s*/\s*')

# WITH expressions should stay together: "Apache-2.0 WITH LLVM-exception"
WITH_RE = re.compile(r'(\S+\s+WITH\s+\S+)')


def extract_spdx_atoms(expressions: list[str]) -> set[str]:
    """Extract individual SPDX identifiers from a list of license expressions.

    Returns the set of atomic identifiers needed for the RPM License: tag.
    WITH expressions (e.g., "Apache-2.0 WITH LLVM-exception") are kept as units.
    """
    atoms = set()
    for expr in expressions:
        if not expr:
            continue

        # Preserve WITH expressions as single atoms
        with_matches = WITH_RE.findall(expr)
        for wm in with_matches:
            atoms.add(wm)

        # Remove WITH expressions before splitting
        clean = WITH_RE.sub('', expr)

        # Normalize / to OR
        clean = SLASH_RE.sub(' OR ', clean)

        # Split on AND, OR, strip parens
        tokens = re.split(r'\s+(?:AND|OR)\s+', clean)
        for tok in tokens:
            tok = tok.strip().strip('()')
            if tok and tok not in ('', 'AND', 'OR', 'WITH'):
                atoms.add(tok)

    return atoms


def build_license_tag(atoms: set[str], project_license: str = "BUSL-1.1") -> str:
    """Build the RPM License: tag from a set of SPDX atoms.

    Joins with AND per Fedora guidelines (all licenses present in the package).
    """
    all_licenses = set(atoms)
    all_licenses.add(project_license)
    return " AND ".join(sorted(all_licenses))


# ---------------------------------------------------------------------------
# Vendor crate extraction
# ---------------------------------------------------------------------------

def read_crate_info_from_dir(vendor_dir: Path) -> list[tuple[str, str, str]]:
    """Read (name, version, license) from vendor/*/Cargo.toml files."""
    crates = []
    for cargo_toml in sorted(vendor_dir.glob("*/Cargo.toml")):
        data = tomllib.loads(cargo_toml.read_text())
        pkg = data.get("package", {})
        name = pkg.get("name", cargo_toml.parent.name)
        version = pkg.get("version", "0.0.0")
        # Handle workspace inheritance marker
        if isinstance(version, dict):
            version = version.get("workspace", "0.0.0")
        license_expr = pkg.get("license", "")
        if isinstance(license_expr, dict):
            license_expr = license_expr.get("workspace", "")
        crates.append((name, version, license_expr))
    return crates


def read_crate_info_from_tarball(tarball: Path) -> list[tuple[str, str, str]]:
    """Read (name, version, license) from vendor/*/Cargo.toml inside a tarball."""
    crates = []
    # Match: prefix/vendor/CRATE_NAME/Cargo.toml (not nested deeper)
    cargo_toml_re = re.compile(r'^[^/]+/vendor/[^/]+/Cargo\.toml$')

    with tarfile.open(tarball, 'r:*') as tf:
        for member in tf.getmembers():
            if not cargo_toml_re.match(member.name):
                continue
            f = tf.extractfile(member)
            if f is None:
                continue
            data = tomllib.loads(f.read().decode('utf-8'))
            pkg = data.get("package", {})
            # Derive crate dir name for fallback
            dir_name = member.name.split('/')[-2]
            name = pkg.get("name", dir_name)
            version = pkg.get("version", "0.0.0")
            if isinstance(version, dict):
                version = version.get("workspace", "0.0.0")
            license_expr = pkg.get("license", "")
            if isinstance(license_expr, dict):
                license_expr = license_expr.get("workspace", "")
            crates.append((name, version, license_expr))

    return sorted(crates, key=lambda x: (x[0].lower(), x[1]))


# ---------------------------------------------------------------------------
# Output generators
# ---------------------------------------------------------------------------

def sanitize_rpm_version(version: str) -> str:
    """Convert SemVer version to RPM-compatible version string.

    RPM uses '-' as the version-release separator, so hyphens in SemVer
    pre-release or build metadata cause parse errors. Convert:
      - SemVer pre-release separator '-' to RPM tilde '~' (sorts before release)
      - Hyphens in build metadata (after '+') to dots
    This matches rust2rpm behavior.
    """
    if '+' in version:
        base, metadata = version.split('+', 1)
        # Sanitize hyphens in build metadata
        metadata = metadata.replace('-', '.')
        version = f"{base}+{metadata}"

    # Handle pre-release hyphens (e.g., 0.6.0-rc.2 -> 0.6.0~rc.2)
    # Only the FIRST hyphen is the pre-release separator in SemVer
    if '-' in version:
        version = version.replace('-', '~', 1)
        # Any remaining hyphens in the pre-release tag become dots
        version = version.replace('-', '.')

    return version


def format_provides(crates: list[tuple[str, str, str]]) -> str:
    """Format Provides: bundled(crate(...)) lines."""
    lines = []
    for name, version, _ in crates:
        rpm_version = sanitize_rpm_version(version)
        lines.append(f"Provides:       bundled(crate({name})) = {rpm_version}")
    return "\n".join(lines)


def update_spec(spec_path: Path, provides_text: str, license_tag: str, crate_count: int):
    """Replace the Provides section and License tag in an RPM spec file.

    Replaces:
    - The License: tag line
    - The comment "# Bundled crate provides..." and all subsequent Provides: lines
    """
    content = spec_path.read_text()

    # Replace License tag
    content = re.sub(
        r'^License:\s+.*$',
        f'License:        {license_tag}',
        content,
        count=1,
        flags=re.MULTILINE,
    )

    # Replace the bundled crate Provides block
    # Pattern: comment line with "crate provides" followed by Provides: lines until a blank line
    provides_block = (
        f"# Bundled crate provides ({crate_count} vendored Rust crates)\n"
        f"{provides_text}"
    )

    # Match from the "# Bundled crate provides" comment through all Provides: lines
    content = re.sub(
        r'^# Bundled crate provides.*\n(?:Provides:.*\n)*',
        provides_block + "\n",
        content,
        count=1,
        flags=re.MULTILINE,
    )

    # Also update the "Vendored Rust crates:" comment if present
    content = re.sub(
        r'# Vendored Rust crates: \d+ crates',
        f'# Vendored Rust crates: {crate_count} crates',
        content,
    )

    spec_path.write_text(content)


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(
        description="Generate RPM Provides: bundled(crate(...)) and License tag"
    )
    parser.add_argument(
        "--tarball", type=Path, default=None,
        help="Source tarball to read vendor/ from (alternative to --vendor-dir)",
    )
    parser.add_argument(
        "--vendor-dir", type=Path, default=None,
        help="Path to vendor/ directory (default: auto-detect from project root)",
    )
    parser.add_argument(
        "--update-spec", type=Path, default=None,
        help="RPM spec file to update in-place",
    )
    parser.add_argument(
        "--project-license", default="BUSL-1.1",
        help="SPDX identifier for the project's own license (default: BUSL-1.1)",
    )
    parser.add_argument(
        "--provides-only", action="store_true",
        help="Only output Provides: lines (no License tag)",
    )
    parser.add_argument(
        "--license-only", action="store_true",
        help="Only output the License: tag",
    )
    args = parser.parse_args()

    # Read crate info
    if args.tarball:
        if not args.tarball.exists():
            print(f"Error: tarball not found: {args.tarball}", file=sys.stderr)
            sys.exit(1)
        print(f"Reading vendor crates from tarball: {args.tarball}", file=sys.stderr)
        crates = read_crate_info_from_tarball(args.tarball)
    else:
        vendor_dir = args.vendor_dir
        if vendor_dir is None:
            # Auto-detect: walk up from script to find vendor/
            script_dir = Path(__file__).resolve().parent
            project_dir = script_dir
            while project_dir != project_dir.parent:
                if (project_dir / "vendor").is_dir():
                    vendor_dir = project_dir / "vendor"
                    break
                project_dir = project_dir.parent
            if vendor_dir is None:
                print("Error: no vendor/ directory found. Use --tarball or --vendor-dir.", file=sys.stderr)
                sys.exit(1)
        print(f"Reading vendor crates from: {vendor_dir}", file=sys.stderr)
        crates = read_crate_info_from_dir(vendor_dir)

    if not crates:
        print("Error: no crates found", file=sys.stderr)
        sys.exit(1)

    print(f"Found {len(crates)} vendored crates", file=sys.stderr)

    # Build outputs
    provides_text = format_provides(crates)
    license_atoms = extract_spdx_atoms([lic for _, _, lic in crates])
    license_tag = build_license_tag(license_atoms, args.project_license)

    # Report
    print(f"License atoms ({len(license_atoms) + 1} including project):", file=sys.stderr)
    for atom in sorted(license_atoms | {args.project_license}):
        print(f"  {atom}", file=sys.stderr)

    # Update spec or print
    if args.update_spec:
        if not args.update_spec.exists():
            print(f"Error: spec file not found: {args.update_spec}", file=sys.stderr)
            sys.exit(1)
        update_spec(args.update_spec, provides_text, license_tag, len(crates))
        print(f"Updated {args.update_spec} with {len(crates)} Provides and License tag", file=sys.stderr)
    else:
        if not args.license_only:
            print(provides_text)
        if not args.provides_only:
            if not args.license_only:
                print()
            print(f"License:        {license_tag}")

    # Summary
    print(f"\nSummary:", file=sys.stderr)
    print(f"  Crates: {len(crates)}", file=sys.stderr)
    print(f"  License atoms: {len(license_atoms) + 1}", file=sys.stderr)


if __name__ == "__main__":
    main()
