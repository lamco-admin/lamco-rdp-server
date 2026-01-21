# lamco-rdp-server Development SOP

**Standard Operating Procedures for Development Sessions**

---

## Repository Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         REPOSITORY ARCHITECTURE                              │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  ~/lamco-rdp-server-dev     │  Development source code (THIS REPO)         │
│  (private)                  │  - All Rust source code                      │
│                             │  - Bundled crates                            │
│                             │  - Tests and benchmarks                      │
│                             │  - DO ALL DEVELOPMENT WORK HERE              │
│                                                                             │
│  ~/lamco-rdp-server         │  Public releases (DO NOT EDIT DIRECTLY)      │
│  (public)                   │  - Receives exports from dev repo            │
│                             │  - Tagged releases only                      │
│                             │  - Updated via export-to-public.sh           │
│                                                                             │
│  ~/lamco-admin              │  Pipeline orchestration (NO APP CODE)        │
│  (private)                  │  - Build/test/publish scripts                │
│                             │  - Packaging (Flatpak manifests, RPM specs)  │
│                             │  - Staging directory for artifacts           │
│                             │  - Project documentation                     │
│                                                                             │
│  ~/wayland/wrd-server-specs │  ARCHIVED - Do not use for new development   │
│                             │  - Historical reference                      │
│                             │  - VDI project code (future)                 │
│                             │  - Architecture documentation                │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Critical Rules

### 1. ALWAYS Work in lamco-rdp-server-dev

All code changes, bug fixes, and feature development MUST happen in this repository (`~/lamco-rdp-server-dev`).

**NEVER directly edit:**
- `~/lamco-rdp-server` (public repo - updated only via pipeline)
- `~/wayland/wrd-server-specs` (archived)

### 2. No Packaging in Dev Repo

This repository contains SOURCE CODE ONLY:
- ✅ `src/` - Rust source code
- ✅ `bundled-crates/` - Locally bundled dependencies
- ✅ `tests/` - Integration tests
- ✅ `benches/` - Benchmarks
- ✅ `Cargo.toml`, `Cargo.lock`

**NOT in this repo:**
- ❌ Flatpak manifests (in lamco-admin/projects/lamco-rdp-server/packaging/)
- ❌ RPM/DEB specs (in lamco-admin)
- ❌ Vendor directory (generated during build)
- ❌ Build artifacts

### 3. Version Management

Version is defined in `Cargo.toml`. When releasing:
1. Update version in `Cargo.toml`
2. Commit: `git commit -am "Release vX.Y.Z"`
3. Tag: `git tag vX.Y.Z`
4. Push: `git push && git push --tags`

---

## Development Workflow

### Starting a Session

```bash
cd ~/lamco-rdp-server-dev
git pull origin main
```

### Building

```bash
# Development build
cargo build

# With GUI
cargo build --features gui

# Release build
cargo build --release

# Flatpak-compatible (no PAM, no wlr-direct)
cargo build --no-default-features --features "h264,libei,gui"
```

### Testing

```bash
cargo test
cargo test -- --nocapture  # With output
cargo bench                 # Benchmarks
```

### Feature Flags

| Feature | Description | Default |
|---------|-------------|---------|
| `h264` | OpenH264 video encoding | ✅ |
| `pam-auth` | PAM authentication | ✅ |
| `gui` | Configuration GUI | ❌ |
| `wayland` | wlr-direct protocols | ❌ |
| `libei` | Portal + EIS input | ❌ |
| `vaapi` | VA-API hardware encoding | ❌ |
| `nvenc` | NVENC hardware encoding | ❌ |

### Committing

```bash
git add -A
git commit -m "Description of changes"
git push origin main
```

---

## ⚠️ CRITICAL: Approval Gates

**NO WRITES TO PUBLIC REPOSITORIES WITHOUT EXPLICIT HUMAN APPROVAL**

### Mandatory Gates

| Gate | Script | What It Does |
|------|--------|--------------|
| GATE-PUB | `export-to-public.sh` | Push source to public repo |
| GATE-REL | `github-release.sh` | Create GitHub release |
| GATE-OBS | `trigger-obs.sh` | Submit to OBS |
| GATE-HUB | Manual only | Flathub PR |

### Enforcement

All publish scripts will:
1. **Block** if no test results exist
2. **Show** pre-publish checklist
3. **Require** typing "yes" (not just Enter)
4. **Log** approval to `staging/v{VERSION}/approval.log`

### Pre-Publish Checklist

```
□ All tests passed (test-results/ reviewed)
□ Test report manually reviewed by human
□ Version number correct in Cargo.toml
□ CHANGELOG updated with release notes
□ No secrets/credentials in staged files
□ SHA256 checksums generated and verified
□ Human explicitly types "yes" to approve
```

---

## Release Process

### Quick Release

```bash
# 1. In dev repo - tag the release
cd ~/lamco-rdp-server-dev
# Update version in Cargo.toml first!
git commit -am "Release v1.0.1"
git tag v1.0.1
git push && git push --tags

# 2. In lamco-admin - build, test, publish
cd ~/lamco-admin
./pipelines/lamco-rdp-server/build/create-vendor-tarball.sh 1.0.1
./pipelines/lamco-rdp-server/build/build-flatpak.sh 1.0.1
./pipelines/lamco-rdp-server/test/deploy-and-test.sh 1.0.1
# If tests pass:
./pipelines/lamco-rdp-server/publish/github-release.sh 1.0.1
./pipelines/lamco-rdp-server/publish/export-to-public.sh 1.0.1
```

### Testing Flatpak Locally

```bash
# Build Flatpak from staging
cd ~/lamco-admin
./pipelines/lamco-rdp-server/build/create-vendor-tarball.sh 0.9.0
./pipelines/lamco-rdp-server/build/build-flatpak.sh 0.9.0

# Install and test
flatpak install --user staging/lamco-rdp-server/v0.9.0/lamco-rdp-server-0.9.0.flatpak
flatpak run io.lamco.rdp-server -vv
```

---

## Directory Structure

```
lamco-rdp-server-dev/
├── src/
│   ├── main.rs              # CLI entry point
│   ├── lib.rs               # Library root
│   ├── gui/                 # GUI application
│   │   ├── main.rs          # GUI entry point
│   │   ├── app.rs           # Main application
│   │   └── tabs/            # Configuration tabs
│   ├── server/              # Core server
│   ├── rdp/                 # RDP channels
│   ├── egfx/                # Video encoding
│   ├── clipboard/           # Clipboard sync
│   ├── damage/              # Damage detection
│   ├── session/             # Session persistence
│   ├── compositor/          # Compositor detection
│   ├── capabilities/        # Capability probing
│   └── ...
├── bundled-crates/
│   ├── lamco-clipboard-core/
│   └── lamco-rdp-clipboard/
├── tests/
├── benches/
├── Cargo.toml
├── Cargo.lock
├── LICENSE
├── README.md
└── SOP.md                   # This file
```

---

## Dependencies

### Published Crates (crates.io)
- `lamco-wayland`, `lamco-rdp`, `lamco-portal`
- `lamco-pipewire`, `lamco-video`, `lamco-rdp-input`

### Bundled Crates (local path)
- `lamco-clipboard-core` - Bundled because it depends on IronRDP fork
- `lamco-rdp-clipboard` - Bundled because it depends on IronRDP fork

### Forked Dependencies
- **IronRDP**: `https://github.com/lamco-admin/IronRDP` (EGFX, clipboard)
- **openh264-rs**: `https://github.com/lamco-admin/openh264-rs` (MSRV fix)

All forks are patched via `[patch.crates-io]` in Cargo.toml.

---

## Common Tasks

### Add a New Feature
1. Create feature branch (optional): `git checkout -b feature/name`
2. Implement in `src/`
3. Add tests in `tests/`
4. Test: `cargo test`
5. Commit and push

### Fix a Bug
1. Reproduce and identify root cause
2. Fix in `src/`
3. Add regression test
4. Test: `cargo test`
5. Commit with descriptive message

### Update a Dependency
1. Edit `Cargo.toml`
2. Run `cargo update -p <package>`
3. Test: `cargo build && cargo test`
4. Commit both `Cargo.toml` and `Cargo.lock`

---

## Troubleshooting

### Build Fails with Missing Dependency
```bash
# Ensure system deps installed (Fedora)
sudo dnf install pipewire-devel openssl-devel pam-devel clang-devel

# Ensure nasm for OpenH264 optimization
sudo dnf install nasm
```

### IronRDP Trait Conflicts
The bundled clipboard crates MUST use the same IronRDP fork as the main crate. If you see trait conflicts, ensure all IronRDP references point to the same git commit.

### GUI Won't Start
- Ensure `--features gui` is enabled
- Check for Wayland session: `echo $WAYLAND_DISPLAY`
- In VM: May need software rendering fallback

---

## Contact

- **Repository**: https://github.com/lamco-admin/lamco-rdp-server-dev
- **Pipeline**: ~/lamco-admin/pipelines/lamco-rdp-server/
- **Documentation**: ~/lamco-admin/projects/lamco-rdp-server/
