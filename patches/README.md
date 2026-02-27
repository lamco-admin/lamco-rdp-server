# Vendor Patches

Patches applied to vendored dependencies during tarball creation.
Each patch has a header documenting its purpose and removal criteria.

These patches are applied automatically by `create-vendor-tarball.sh`
after `cargo vendor` and before creating the tarball.

## Active Patches

| Patch | Target Crate | Reason | Remove When |
|-------|-------------|--------|-------------|
| `cros-libva-vp9-default.patch` | cros-libva | VP9 struct fields from newer libva | cros-libva upstream updates |

## Removed Patches

| Patch | Removed | Reason |
|-------|---------|--------|
| `ironrdp-graphics-cast-signed.patch` | 2026-02-23 | MSRV raised to 1.88; all targets have cast_signed() |

## Adding a New Patch

1. Create the `.patch` file with the header block (see existing patches for format)
2. The `Target:` header line specifies the file path relative to the build directory
3. `create-vendor-tarball.sh` applies all `.patch` files automatically
4. After adding, test with a fresh vendor tarball build
