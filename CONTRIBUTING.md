# Contributing to lamco-rdp-server

Thank you for your interest in contributing to lamco-rdp-server!

---

## Before Contributing

Please note that lamco-rdp-server is licensed under the **Business Source License 1.1 (BUSL-1.1)**, which means:

- The code is **source-available** but not fully open source
- Free use is granted for single-server deployments and non-profits (see LICENSE for details)
- Commercial use by larger organizations requires a license
- The code will automatically convert to **Apache License 2.0** on **December 31, 2028**

**By contributing, you agree that your contributions will be licensed under the same terms.**

---

## How to Contribute

### Reporting Issues

**Before opening an issue:**
1. Check existing issues to avoid duplicates
2. Run diagnostics: `lamco-rdp-server --diagnose`
3. Gather version info: `lamco-rdp-server --version`
4. Include log output if relevant

**Good bug reports include:**
- Your Linux distribution and version
- Desktop environment (GNOME, KDE, wlroots compositor)
- Steps to reproduce
- Expected behavior
- Actual behavior
- Relevant log excerpts

### Suggesting Features

**Feature requests should include:**
- Use case description
- Why existing features don't solve this need
- Proposed implementation approach (if you have ideas)
- Willingness to contribute code (if applicable)

**Note:** We prioritize features that:
- Work across multiple compositors (not GNOME-only or KDE-only)
- Align with Wayland/Portal security model
- Don't break existing functionality

---

## Development Setup

### Prerequisites

```bash
# Install Rust 1.77+
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install dependencies (Ubuntu/Debian)
sudo apt install nasm libssl-dev libpipewire-0.3-dev libva-dev \
  libwayland-dev libxkbcommon-dev libdbus-1-dev libpam0g-dev

# Install dependencies (Fedora/RHEL)
sudo dnf install nasm openssl-devel pipewire-devel libva-devel \
  wayland-devel libxkbcommon-devel dbus-devel pam-devel
```

### Clone and Build

```bash
git clone https://github.com/lamco-admin/lamco-rdp-server
cd lamco-rdp-server

# Build
cargo build

# Run tests
cargo test

# Run with verbose logging
cargo run -- -c config.toml -vvv
```

### Code Style

**We follow standard Rust conventions:**

```bash
# Format code
cargo fmt

# Check for issues
cargo clippy --all-features

# Both must pass before submitting PR
```

**Linting standards:**
- No `unsafe` code without justification
- All public APIs must have documentation
- Add tests for new functionality
- Keep functions focused and readable

---

## Submitting Pull Requests

### Before Submitting

- [ ] Code formatted with `cargo fmt`
- [ ] No clippy warnings: `cargo clippy --all-features -- -D warnings`
- [ ] All tests pass: `cargo test`
- [ ] New code has documentation
- [ ] Added tests for new features
- [ ] Updated CHANGELOG.md if user-visible change

### PR Guidelines

**Good PRs:**
- Focus on a single feature or fix
- Include clear commit messages
- Reference related issues (if any)
- Include test coverage
- Update documentation as needed

**PR Description Should Include:**
- What problem does this solve?
- How does it solve it?
- Are there alternatives you considered?
- Any breaking changes?
- Testing performed

### Commit Messages

Follow conventional commits format:

```
feat(clipboard): add support for HTML format conversion
fix(egfx): correct color space conversion for AVC444
docs(readme): clarify wlroots support requirements
refactor(session): simplify token storage logic
```

Types: `feat`, `fix`, `docs`, `refactor`, `test`, `chore`

---

## Areas Where Contributions Are Welcome

### High Priority

- **Distribution testing**: Test on untested distros (see `docs/DISTRO-TESTING-MATRIX.md`)
- **Bug fixes**: Especially Portal-related edge cases
- **Performance improvements**: Encoding optimizations, bandwidth reduction
- **Documentation**: Usage examples, troubleshooting guides

### Medium Priority

- **Hardware encoding**: NVENC improvements, additional VA-API profiles
- **Codec support**: Additional video codecs
- **Clipboard formats**: Additional format conversions
- **Input handling**: International keyboard layouts, special keys

### Lower Priority

- **New compositors**: Support for additional Wayland compositors
- **Audio redirection**: RDP audio channel (future feature)
- **File system redirection**: Drive mapping (future feature)

---

## Code of Conduct

**Be respectful:**
- Assume good intent
- Provide constructive feedback
- Focus on the code, not the person
- Welcome newcomers

**Be collaborative:**
- Discuss major changes before coding
- Review others' contributions thoughtfully
- Help answer questions in issues

**Unacceptable behavior:**
- Personal attacks or harassment
- Discriminatory language
- Spam or off-topic discussions

Violations may result in bans from the project.

---

## Development Resources

**Useful documentation:**
- `docs/architecture/` - System architecture deep-dives
- `docs/SESSION-PERSISTENCE-ARCHITECTURE.md` - Session persistence design
- `docs/SERVICE-REGISTRY-TECHNICAL.md` - Service registry implementation
- `docs/WLR-FULL-IMPLEMENTATION.md` - wlroots support details
- `docs/DISTRO-TESTING-MATRIX.md` - Compatibility testing status

**External documentation:**
- [IronRDP](https://github.com/Devolutions/IronRDP) - RDP protocol implementation
- [XDG Desktop Portal](https://flatpak.github.io/xdg-desktop-portal/) - Portal API reference
- [PipeWire](https://docs.pipewire.org/) - Screen capture API
- [Wayland Protocols](https://wayland.app/) - Wayland protocol reference

---

## Questions?

**For development questions:**
- Open a discussion on GitHub
- Review existing issues and PRs
- Check documentation in `docs/`

**For commercial licensing:**
- Email: office@lamco.io
- Website: https://lamco.ai

---

**Thank you for contributing to lamco-rdp-server!**
