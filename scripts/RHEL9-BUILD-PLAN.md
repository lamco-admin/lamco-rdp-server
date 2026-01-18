# RHEL 9 Build and Test Plan

**Target:** 192.168.10.6 (RHEL 9.7 with GNOME 40.10)
**Purpose:** Build with glibc 2.34, test Mutter strategy on GNOME 40
**Blocker Resolved:** Build on RHEL 9 directly (not cross-compile)

---

## Why RHEL 9?

**glibc Compatibility:**
- Ubuntu 24.04 binary: requires glibc 2.39
- RHEL 9: has glibc 2.34
- **Can't run Ubuntu binary on RHEL 9**

**Critical Testing Target:**
- GNOME 40.10 (older than tested GNOME 46)
- Portal v4 (has restore tokens)
- Mutter D-Bus services present
- **The question:** Does Mutter actually work on GNOME 40?

---

## Prerequisites (Already Met ✅)

**RHEL 9 VM:**
- ✅ IP: 192.168.10.6
- ✅ User: greg / Pass: Bibi4189
- ✅ Rust 1.92.0 installed
- ✅ Cargo installed
- ✅ GNOME 40.10 running
- ✅ Portal services available
- ✅ Mutter D-Bus APIs present

**Local Machine:**
- ✅ sshpass installed (for automation)
- ✅ All source code ready

---

## Directory Structure to Copy

```
/home/greg/wayland/
├── wrd-server-specs/       1.7GB source (main project)
├── IronRDP/                  35MB source (fork with EGFX/clipboard)
├── lamco-wayland/           2.8MB source (lamco-portal)
├── lamco-rdp-workspace/     3.0MB source (pipewire, clipboard)
└── openh264-rs/              31MB source (VUI support)

Total: ~1.77GB source only (target/ excluded)
```

**RHEL 9 Target:**
```
/home/greg/wayland-build/
├── wrd-server-specs/
├── IronRDP/
├── lamco-wayland/
├── lamco-rdp-workspace/
└── openh264-rs/
```

---

## Deployment Process

### Step 1: Run Deployment Script

```bash
cd /home/greg/wayland/wrd-server-specs
./scripts/deploy-to-rhel9.sh
```

**What it does:**
1. Creates `/home/greg/wayland-build/` on RHEL 9
2. Rsyncs all 5 directories (excludes target/, .git/, logs)
3. Creates build.sh on RHEL 9
4. Reports completion

**Time:** 5-10 minutes (depends on network)

### Step 2: SSH to RHEL 9

```bash
ssh greg@192.168.10.6
# Password: Bibi4189
```

### Step 3: Build

```bash
cd ~/wayland-build
./build.sh
```

**What build.sh does:**
1. Changes to wrd-server-specs/
2. Runs `cargo build --release`
3. Reports binary location and info
4. Shows glibc linkage

**Time:** 5-15 minutes (first build, Rust will download crates)

### Step 4: Verify Binary

```bash
cd ~/wayland-build/wrd-server-specs
./target/release/lamco-rdp-server --help
./target/release/lamco-rdp-server --show-capabilities
```

**Expected:**
- Binary runs
- Shows help text
- Shows detected capabilities:
  - Compositor: GNOME 40.10
  - Portal: v4
  - Deployment: Native Package
  - Credential Storage: (depends on what's available)

---

## Path Dependencies Resolution

**Current Cargo.toml paths:**
```toml
lamco-portal = { path = "../lamco-wayland/crates/lamco-portal" }
ironrdp = { path = "/home/greg/wayland/IronRDP/crates/ironrdp" }
openh264 = { path = "/home/greg/openh264-rs/openh264" }
```

**On RHEL 9 after rsync:**
```
/home/greg/wayland-build/
├── wrd-server-specs/
│   └── Cargo.toml (paths will work if we preserve structure)
├── lamco-wayland/          ← ../lamco-wayland/ works from wrd-server-specs/
├── lamco-rdp-workspace/    ← ../lamco-rdp-workspace/ works
├── IronRDP/                ← Need to fix: was /home/greg/wayland/IronRDP
└── openh264-rs/            ← Need to fix: was /home/greg/openh264-rs
```

**Two options:**

**Option A: Update Cargo.toml on RHEL 9** (simple sed command)
**Option B: Use symlinks** (preserve absolute paths)

Let me add Option A to the build script.

---

## Testing Plan (On RHEL 9)

### Test 1: Check Capabilities

```bash
./target/release/lamco-rdp-server --show-capabilities
```

**Look for:**
- Compositor: GNOME 40.10 ✅
- Portal version: 4 ✅
- DirectCompositorAPI service level: BestEffort or Unavailable?
- Recommended strategy: Mutter or Portal?

### Test 2: Run Server (Foreground)

```bash
./target/release/lamco-rdp-server -p 3389
```

**Watch logs for:**
- Service Registry output
- Strategy selection (Mutter or Portal?)
- Session creation success/failure
- Dialog count (0 or 1?)

### Test 3: Connect with RDP Client

**From another machine:**
```bash
xfreerdp /v:192.168.10.6:3389 /u:test /size:1280x1024
```

**Test:**
- Video: Does screen appear?
- Mouse: Does it align correctly?
- Keyboard: Does typing work?
- Clipboard: Copy/paste both directions?

### Test 4: Restart Test (Token Persistence)

```bash
# Kill server (Ctrl+C)
# Restart:
./target/release/lamco-rdp-server -p 3389
```

**Check:**
- Dialog count (should be 0 if tokens work)
- Token restoration logged?
- Session created without user interaction?

---

## Expected Outcomes

### Outcome A: Mutter Works on GNOME 40 ✅

**What you'll see:**
```
✅ Selected: Mutter Direct D-Bus API strategy
   Zero permission dialogs (not even first time)

Mutter session created successfully (NO DIALOG REQUIRED)
  Stream 0: 1920x1080 at (0, 0), PipeWire node: 59
```

**Then:**
- Video should work
- Mouse should work
- Keyboard should work
- **Confirms:** Mutter is viable for Portal v3 systems

**Next steps:**
- Keep Mutter strategy
- Document GNOME 40-45 support
- Test on Ubuntu 22.04 LTS for confirmation

---

### Outcome B: Mutter Broken on GNOME 40 ❌

**What you'll see:**
```
Service Registry reports Mutter API available, but connection failed
Falling back to Portal + Token strategy

OR

✅ Selected: Mutter Direct D-Bus API strategy
Creating Mutter session...
Error: Failed to create Mutter session: <some error>
```

**Or worse:**
- Mutter session creates but video is black screen
- Mutter session creates but input doesn't work

**Then:**
- Mutter is broken on all GNOME versions (not just 46)
- Disable Mutter entirely
- Simplify to Portal-only
- Remove ~1,100 lines of Mutter code

**Next steps:**
- Update Service Registry to mark all Mutter as Unavailable
- Document Portal-only approach
- Accept one dialog on Portal v3 systems (RHEL 9, Ubuntu 22.04)

---

### Outcome C: Build Fails ❌

**Possible issues:**
- Missing system libraries (pipewire-devel, dbus-devel, etc.)
- Rust version too old (unlikely, 1.92.0 is recent)
- Dependency resolution issues

**Solutions:**
- Install missing packages: `sudo dnf install pipewire-devel dbus-devel glib2-devel`
- Check error messages
- Fix and retry

---

## Quick Reference Commands

### Deploy and Build (One-Liner)

```bash
cd /home/greg/wayland/wrd-server-specs && \
./scripts/deploy-to-rhel9.sh && \
sshpass -p 'Bibi4189' ssh greg@192.168.10.6 "cd ~/wayland-build && ./build.sh"
```

### SSH to RHEL 9

```bash
ssh greg@192.168.10.6
# Or with password:
sshpass -p 'Bibi4189' ssh greg@192.168.10.6
```

### Check Build Results

```bash
sshpass -p 'Bibi4189' ssh greg@192.168.10.6 "ls -lh ~/wayland-build/wrd-server-specs/target/release/lamco-rdp-server"
```

### Copy Binary Back

If you want to keep the RHEL 9 binary locally:
```bash
sshpass -p 'Bibi4189' scp greg@192.168.10.6:~/wayland-build/wrd-server-specs/target/release/lamco-rdp-server \
    /home/greg/wayland/wrd-server-specs/lamco-rdp-server-rhel9
```

---

## Timeline Estimate

**Deployment:** 5-10 minutes (rsync transfer)
**First build:** 10-20 minutes (cargo downloads crates, compiles)
**Testing:** 15-30 minutes (run server, connect, verify features)

**Total:** ~30-60 minutes to answer "Does Mutter work on GNOME 40?"

---

## Success Criteria

**Minimum Success:**
- [ ] Binary builds on RHEL 9
- [ ] Binary runs (--help works)
- [ ] Capabilities detected correctly
- [ ] Some strategy works (Mutter OR Portal)

**Full Success:**
- [ ] Binary builds
- [ ] Binary runs
- [ ] Mutter strategy selected (if service registry allows)
- [ ] Video works (screen visible)
- [ ] Mouse works (correct alignment)
- [ ] Keyboard works
- [ ] Zero dialogs (Mutter) or one dialog (Portal fallback)

---

## Fallback Plan

If rsync is too slow or fails:

**Option: tar + scp**
```bash
# Create tarball locally
cd /home/greg/wayland
tar czf wayland-build.tar.gz \
    --exclude='target' \
    --exclude='.git' \
    --exclude='*.log' \
    wrd-server-specs IronRDP lamco-wayland lamco-rdp-workspace openh264-rs

# Copy to RHEL 9
sshpass -p 'Bibi4189' scp wayland-build.tar.gz greg@192.168.10.6:~/

# Extract on RHEL 9
sshpass -p 'Bibi4189' ssh greg@192.168.10.6 "tar xzf wayland-build.tar.gz && mv wrd-server-specs IronRDP lamco-wayland lamco-rdp-workspace openh264-rs wayland-build/"
```

---

*Ready to deploy and test on RHEL 9*
