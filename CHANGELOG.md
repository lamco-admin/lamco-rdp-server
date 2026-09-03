# Changelog

All notable changes to lamco-rdp-server will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

Add entries here as work lands; retitle to the release version and date when the release is cut.

## [1.4.5] - 2026-09-02

### Added

**Area capture on GNOME, so a fullscreen video no longer freezes the session.** Mutter stops recording a monitor stream entirely while a fullscreen or maximised surface is handed to direct scanout: its monitor source copies from the scanout buffer and gives up when it cannot, which froze the remote picture for the length of any video (GNOME/mutter#3903; upstream fix mutter!5276 is approved but unreleased). Mutter's area source re-paints the stage into its own framebuffer instead, and carries no such gate on either GNOME 49 or 50. The server now resolves the monitor's rectangle through `org.gnome.Mutter.DisplayConfig` and records that area. Because an area's rectangle is fixed when the stream is created and Mutter never updates it, the session handle records what it captures, and when a client connects to a still-live session the server re-checks the rectangle against the compositor and rebuilds the session if the host's resolution, scale or layout has moved; a compositor that will not answer is treated as no evidence of change rather than a reason to tear down a working session. An area stands in for exactly one monitor, so a multi-monitor layout, and any connector whose rectangle cannot be resolved, fall back to the monitor stream. Selected by `capture.gnome_record_mode`: `auto` (the default: area on a single monitor where a rectangle resolves, which in practice is every released Mutter today), `area` (prefer area, same fallbacks), or `monitor` (always the monitor stream, accepting the freeze). The costs are real: an area stream always re-composites where a monitor stream blits, Mutter's area source runs at a fixed 60 fps, and the rectangle needs rebuild machinery a monitor stream gets for free. Along the way, an area stream was found to report a 0x0 size in its stream parameters, and advertising a 0x0 desktop made a strict client abandon the connection during finalize; a zero dimension is now treated as absent on every Mutter path and the area the server asked for is used instead. The trade-offs, the evidence, and the open question of whether this is a workaround or the right architecture are set out in `docs/decisions/GNOME-CAPTURE-SOURCE-AREA-VS-MONITOR-2026-08-30.md`.

**VA-API hardware encoding is now used for EGFX H.264, both AVC444 and AVC420.** Hardware encoding had been configurable for some time and the encoder factory existed, but the display pipeline never called it, so every EGFX session encoded in software with OpenH264 regardless of the setting; the GUI's own note on the toggle said as much. On a build with the `vaapi` feature and `hardware_encoding.enabled` set (off by default), both the AVC444 encoder, which is the path most Windows clients including mstsc negotiate, and the AVC420 encoder now prefer a VA-API GPU encoder and fall back to software OpenH264 when one cannot be built, whether because there is no VA-API, the driver exposes no H.264 encode entrypoint, or the factory produced a different backend. The decision is made when the encoder is constructed, so a failure falls back before the first frame rather than mid-stream, and the log says which path was chosen. Two constraints shaped the implementation: MS-RDPEGFX requires the AVC444 main and aux subframes to share one encoder and one DPB, so the hardware encoder gained an NV12 entry point that takes a pre-formed view and an explicit keyframe decision; and VA-API handles are thread-affine, so a dedicated encoder thread services BGRA and NV12 requests one at a time in order, which is also what keeps the AVC444 DPB unified. Only VA-API implements the dual-view entry point today, so NVENC and Vulkan Video builds still fall back to software for now. The GUI notes now say the toggle takes effect on the next connection and needs a VA-API driver with H.264 encode plus libva-utils. Performance telemetry and the GUI Performance tab also name the active encoder backend (`openh264` or `vaapi`), where the field was previously always empty.

**Server-driven cursor shape and position, so the client draws the pointer itself.** When the capture stream carries cursor metadata, the server now turns it into RDP pointer updates instead of ignoring it, on both the EGFX and the RemoteFX/bitmap paths. A shape change is detected from the compositor's cursor id, so most frames are position-only; on a real change the bitmap is converted to a New Pointer Update (shapes up to 96x96) or a Fast-Path Large Pointer Update (up to the protocol's 384x384 ceiling, gated by what the client negotiated through the Large Pointer Capability Set), and a round-robin cache of eight slots keyed by cursor id lets a recurring shape go out as a cached pointer instead of a full re-encode. Position is mapped through the same coordinate transformer the input handler uses in the opposite direction, so monitor lookup, DPI scaling and virtual-desktop normalization match input; a hidden cursor sends a single hide per hidden span. The Android Microsoft RD Client renders the spec's bottom-up XOR rows flipped, so shapes appeared upside down there; that client is detected by its `AVC_DISABLED` EGFX capability flag and sent top-down rows. The `[cursor]` config section previously reached nothing outside the GUI editor; it now builds a live cursor strategy. `cursor.auto_mode` was meant to switch to predictive mode once measured latency exceeded `cursor.predictive_latency_threshold_ms` (100 ms by default), but nothing ever fed it a latency; the server now reuses the NetworkAutoDetect round-trip time it already measures, ticks at `cursor.cursor_update_fps` (60 by default), and in predictive mode re-emits the pointer position at the predicted coordinates between real cursor samples, so on a high-latency link the cursor switches to prediction and moves smoothly, and on a low-latency link nothing changes. `cursor.mode = "painted"` (server-side compositing) is accepted but behaves like metadata until compositing exists.

**Real multitouch input over MS-RDPEI on the libei input path, off by default.** Touch from an RDP client previously reached the desktop only as mouse emulation. Touch contact frames from the RDPEI dynamic virtual channel are now queued onto the same batching pipeline the mouse and keyboard use, tracked through a per-contact state machine matching MS-RDPEI's down, hover and engaged model, and injected through the EIS strategy's touchscreen device. Touch frames are never coalesced or dropped, because losing a down or up transition would leave a contact stuck engaged, and touch state is reset on reconnection. Injection works only on the EIS/libei strategy; other strategies keep the no-op default. Gated on `input.enable_touch` (the "Enable Touch Input" GUI toggle, default off): when off the RDPEI channel is not offered to the client at all. Pen input has no injection path in this stack and keeps its log-only default.

**The client's negotiated keyboard layout is applied to the session.** RDP clients announce their keyboard layout during connection and the server ignored it, so every session ran the scancode mapper as a US layout whatever the client had. The layout is now received on connection and applied to the session's keyboard handler for the layouts with real override tables (US, UK, German, French, Belgian, Italian, Spanish, Portuguese); anything else keeps the US default, including CJK, which arrives as Unicode keyboard events rather than scancodes. Separately, the portal-generic strategy never implemented keysym injection, so every non-ASCII Unicode keystroke on that path was silently dropped; it now resolves the keysym through the backend's own keymap. wlr-direct still cannot inject keysyms because `zwp_virtual_keyboard_v1` has no such request, and the strategy now documents that in place.

**Round-trip time and bandwidth are measured and reported to the client.** RDP's NetworkAutoDetect lets a server probe the connection and send the client a Network Characteristics Result, which clients such as mstsc and Remote Desktop Manager use to size their receive and jitter buffers. Nothing in the desktop session paths drove it before, so a desktop session measured nothing and sent nothing. Both desktop session paths now enable auto-detect, share the RTT handle with the EGFX flow controller as a freshness floor for its own estimate, and probe every 250 ms while the EGFX channel is ready. The result carries base RTT, bandwidth and average RTT; the server side of that was filed upstream to IronRDP and is now inherited from upstream master.

**HTML and image pastes from Windows now work on the Wayland data-control clipboard path.** On compositors served by the data-control provider (`ext-data-control-v1` or `wlr-data-control-v1`, no portal involved), the compositor's send is synchronous, so the server must already hold the bytes when asked for them. The eager fetch on a Windows copy covered only plain text and file lists, which left HTML and images announced with an empty source behind them. The announcement now also queues CF_HTML (published as `text/html`) and one image format, preferring PNG, then DIB converted to PNG, then JPEG and GIF as passthrough, chained one request at a time since IronRDP allows one outstanding data request. Both are gated by the HTML and image entries in `clipboard.allowed_types`, which previously nothing consumed, and decoded images larger than `clipboard.max_size` (10 MB by default) are not published rather than served partially.

**Capture protocol, monitoring and notification settings are editable in the GUI.** Three config sections were fully functional server-side but had no GUI representation. `[capture]` (the Wayland capture protocol for the portal-generic strategy, fallback, and handshake timeout) appears on the Video tab; `[monitoring]` (performance snapshots and the Prometheus and `/health` bind address, active only with the `metrics-server` feature) under Performance; `[notifications]` (desktop notifications on server error and on TLS certificate expiry) on the Status tab. `[diagnostics]` stays config-only by design, since the decode self-test and H.264 dump cost real CPU and disk.

**Wayland color and display observers.** Two server-lifetime Wayland clients read live compositor state so the encode and resize paths can act on what the display is doing instead of inferring it. A `wp_color_management_v1` observer binds each output and reads its image description (transfer function SDR, PQ or HLG, primaries, luminances, and mastering-display metadata) into a live per-output snapshot, re-reading on change. A `zwlr_output_management_v1` observer reads every wlroots output head and its modes and can apply a configuration. Both are read-side only: the server captures through PipeWire and owns no surface. The color observer starts only where the compositor advertises `wp_color_manager_v1`, and the output observer only on wlroots-family compositors (Sway, Hyprland, or a generic wlroots detection). Three new `[display]` keys, all defaulting to true, gate them: `color_management`, `output_management`, and `resize_drives_output_mode`. When the H.264 encoder is initialised for a client the server now warns if any observed output is HDR, since those pixels would otherwise be squashed to 8-bit SDR silently. The runtime behaviour was not checked against a real compositor when it landed; `docs/analysis/WAYLAND-COLOR-OUTPUT-HDR-VERIFICATION-2026-07-07.md` records what remains to be confirmed.

**Dynamic resolution on wlroots.** On a client-initiated resize, the server now asks a wlroots-family compositor to switch the first enabled output head to a custom mode of the requested width and height through `zwlr_output_management_v1`, so the physical output changes resolution instead of the server only capturing a scaled region. This applies in direct-channel (portal-generic) mode as well, where the request was previously ignored because the compositor output was fixed. It is best-effort and non-fatal: a missing serial or head is logged at debug and the PipeWire capture-side resize still runs. The target is the first enabled head, not necessarily the one being captured, a known multi-monitor gap. Gated by `display.resize_drives_output_mode` and requires `display.output_management`.

**Experimental HDR to SDR tone-mapping (opt-in, off by default).** A standalone, unit-tested DSP module (PQ/HLG EOTF decode, BT.2020 to BT.709 gamut map, Hable filmic tone-map normalised to the source peak, sRGB re-encode). When `display.hdr_tone_mapping` is enabled and the color observer reports an HDR output, captured PQ/HLG frames are tone-mapped to SDR before damage detection and encode. It is off by default because whether a compositor delivers HDR-encoded pixels over the capture stream is compositor-specific; many tone-map to SDR themselves, and applying this to already-SDR pixels washes the image out. Known limits: the HLG path applies no display OOTF, the trigger reads the output's color state rather than the stream's negotiated encoding, and the conversion is per-pixel on the CPU with no SIMD, so it is costly at high resolutions. Requires `display.color_management`.

**Opt-in pure-Rust Opus audio backend.** The RDPSND Opus encoder is built through opus2, which as of 0.4 offers two mutually exclusive encode backends, and the choice is now a Cargo feature. `opus-libopus`, which joins the default feature set, keeps the C libopus backend compiled from vendored source and statically linked, so a default build produces the same encoder as before. `opus-pure-rust` swaps in mousiki, a pure-Rust port of the Xiph reference, which removes the C toolchain from the build and simplifies cross-compilation and static builds; mousiki is young and has limited x86 SIMD, so it encodes more slowly than libopus on x86 and is offered for evaluation and constrained toolchains rather than as the default. Any build that passes `--no-default-features` must now add one of the two explicitly.

**EIS Unicode input and discrete scroll (reis 0.7).** On the libei and Mutter Direct EIS paths, an RDP Unicode keyboard event for a character with no US-QWERTY evdev keycode (CJK, accented letters, emoji) was silently dropped. reis 0.7 exposes libei's `ei_text` interface, and the server now injects such characters by keysym through it when the compositor advertises the text capability (libei 1.6 and later); older compositors behave exactly as before. A Mutter Direct build without the `libei` feature forwards the keysym over Mutter's D-Bus RemoteDesktop instead. Discrete (notch) scroll is now forwarded as exactly one `ei_scroll.scroll_discrete` detent per RDP notch, where it was previously smoothed into a continuous delta of 15 units, so the compositor sees a wheel detent. An intermediate change that paired each notch with `scroll_stop` was found to make libei drop the whole frame, which left Firefox and GNOME applications unable to scroll, and was removed before release.

### Changed

- **Minimum supported Rust is 1.94, and the IronRDP pin is now a pure mirror of upstream master.** The last two fork-local IronRDP commits, mid-session CapsAdvertise decoder recovery and EGFX diagnostic logging, merged upstream on 2026-08-30, so the `lamco-admin/IronRDP` branch was fast-forwarded onto `Devolutions/IronRDP` master at `fb0c0413` with zero fork-local commits, kept under the fork URL by policy. `rust-version` rises from 1.89 to 1.94 to match the floor IronRDP's crates now declare; distributions whose toolchain has not reached 1.94 cannot build this release. Over the cycle the pin was rebased onto upstream several times, which brought in RemoteFX encoder correctness fixes (three RLGR entropy-encoder fixes, one of which had RLGR1 rendering a black screen on Windows 11 mstsc, masked in practice because the server resolves to RLGR3 against mstsc; a regression fix for a phantom coefficient on a trailing zero-run; and an output-buffer overflow that now returns an error instead of panicking), the ICAP entropy-coder weighting fix, server-configurable RemoteFX quantization, EGFX scaled-surface composition, and the connector auto-detect header fix. Upstream's typed-error migration changed the display and connection handler signatures, and ironrdp-server dropped its `tokio_rustls` re-export, so tokio-rustls 0.26 is now a direct dependency. Upstream also landed the RDP-UDP transport stack and an RD Gateway client stack, none of which is wired into the server yet.
- **Dependency refresh.** lamco-pipewire moves from the 0.5.x line to 0.6.13, which requires libpipewire 0.3.62 or newer at build and run time (up from 0.3.33); the 0.6 line carries the cursor bitmap metadata, the stream-tag read path, the corrupted-frame and mappable-buffer diagnostics, and the fix for the deinit crash described under Fixed. lamco-video 0.3.0, lamco-portal 0.4.5, lamco-rdp 0.8.0, lamco-rdp-input 0.2.0 (position on button events and multitouch), lamco-rdp-clipboard 0.5.0, and xdg-desktop-portal-generic 0.6.1 (capture request pacing, EIS frame atomicity and `ready()` gating) move with it. reis 0.6.1 to 0.7.1 (the binary had been compiling two copies of reis; it now resolves one), opus2 0.3.3 to 0.4.0, the Wayland binding stack to wayland-protocols 0.32.13 (which is what exposes the full `wp_color_management_v1` surface), nix to 0.31, zbus to 5.18, ashpd to 0.13.13 in the lockfile with the `0.13.7` floor unchanged, libloading to 0.9, x509-cert to 0.3.0 (collapsing two copies of x509-cert and der to one), the openh264 and openh264-sys2 floors to 0.9.8, and cudarc to 0.19.9, where 0.19.8 adds CUDA 13.3 toolkit support for `--features nvenc`. Security advisories cleared: rustls-webpki 0.103.13 (RUSTSEC-2026-0104, a reachable panic in CRL parsing, and 0098 and 0099, name-constraint bypasses) on the TLS path, cmov 0.5.4 (GHSA-3rjw-m598-pq24), crossbeam-epoch 0.9.20 (RUSTSEC-2026-0204), and h2 0.4.16 (RUSTSEC-2026-0258, unbounded empty DATA frames); anyhow and memmap2 clear two unsound warnings. Still open and acknowledged in `.cargo/audit.toml`: RUSTSEC-2023-0071, the rsa Marvin timing side channel reached transitively through picky and sspi with no upstream fix since 2023, and two quick-xml advisories that need 0.41 and are blocked on wayland-scanner and iced, build-time only.
- **GNOME now attempts DMA-BUF capture, and virtual GPUs are checked for Venus instead of refused by driver name.** The GNOME compositor profile forced shared-memory buffers, a default that predates lamco-pipewire's fix for the all-zero DMA-BUF data it was working around. The profile now recommends any buffer type, so GNOME sessions request DMA-BUF best-effort with shared memory as the fallback; it is not guaranteed because GNOME's DMA-BUF reliability still trails KDE's across driver and version combinations. Separately, the virtual-GPU gate forced shared memory on any virtio driver, which meant a Venus-capable virtio-gpu could never take the DMA-BUF path even though it can back CPU-readable DMA-BUF. The server now asks the kernel directly whether the connected virtio card supports the Venus capset, the same mechanism Mesa's Venus ICD uses, and only forces shared memory when Venus is confirmed absent; the startup line reads "Virtual GPU without Venus detected" when the fallback engages.
- **The GNOME direct-scanout freeze is now named in the log instead of looking like an idle desktop, and no longer floods it.** While a fullscreen surface is in direct scanout, Mutter marks every screencast buffer corrupted (GNOME/mutter#3903). Those buffers were dropped before a frame existed, so nothing downstream could tell the condition from a static desktop, while the capture crate warned once per buffer, about a thousand lines a minute. lamco-pipewire 0.6.11 counts them at the source and rate-limits that warning to the first and every hundredth. When a client is connected, no usable frame has arrived for 1.5 seconds, and at least ten corrupted buffers have accumulated in two seconds, the server reports a `VideoFramesCorrupted` health event and logs one warning naming the compositor condition and the upstream fix. The rate floor exists because corrupted chunks interleave at a low rate on healthy virtio/Venus sessions and an idle desktop legitimately sends nothing for seconds. With area capture (see Added) the freeze no longer occurs in the default single-monitor configuration; this diagnostic remains for monitor mode and the multi-monitor fallback.
- **The compositor's own output scale is now observable.** PipeWire lets a producer annotate a stream with `SPA_PARAM_Tag` key/value pairs, and Mutter publishes the logical monitor's scale there as `org.gnome.scale`. The server reads it every pass and reports it, which is the first time a compositor-provided scale has been visible from the capture side at all; every previous source left it to be inferred. Mutter sets the tag only on a virtual-monitor stream, so today it appears on headless GNOME sessions and nowhere else. Nothing consumes the value for sizing yet: this closes the observation half of the HiDPI gap, and DPI-aware resize remains deferred.
- **Headless GNOME sessions can present their virtual monitor as a real display.** Mutter's `RecordVirtual` accepts an `is-platform` property, which it documents as the output not being "interpreted as if the screen is shared, but more transparently as if it was a real monitor". For a headless server the virtual monitor is the session's only display, so that is the accurate description of it. Enabled with `capture.gnome_virtual_is_platform`, off by default since it changes how the compositor presents the session. Headless sessions only; it needs ScreenCast API version 3 (GNOME 46 and later), and older Mutter ignores the property rather than failing.
- **Faster BGRA to YUV conversion on the software H.264 path.** The AVC420 software encoder converted each frame through openh264's generic `from_rgb_source`, which reads one pixel at a time through a trait call and converts in floating point. It now uses `from_bgra8_source`, the contiguous route openh264 0.9.8 backs with hand-written AVX2 on x86_64, which accepts our BGRA layout directly with no reordering. Output is unchanged: upstream's own test asserts the two routes agree. The `openh264` and `openh264-sys2` floors are raised to 0.9.8 to match, which resolved to the same versions the build was already using.
- **Capability diagnostics recognize `ext-image-copy-capture-v1` as a dialog-free capture path.** The services registry had no notion of the standardized successor to wlr-screencopy, so the Unattended Access service reported "Basic Portal (dialog each time)" on compositors that implement it but not wlr-screencopy (Mir, phoc, Jay, Treeland, labwc, Wayfire 0.11 and later). Session establishment was never affected, since the strategy selector already checked the protocol directly; only the reported capability was wrong, and it now reads "ext-image-copy-capture (no dialog)" there.
- **Klipper cooperation mode warns when the running Plasma may hit the portal-kde clipboard crash.** KDE bug 515465 crashes xdg-desktop-portal-kde one to two seconds after a SetSelection call whenever Klipper reads the clipboard, on Plasma 6.3.90 through 6.5.x, fixed in 6.6.0 and not backported. The cooperation mode still calls SetSelection and is the branch chosen on KDE when no data-control protocol is available, so there is no safer mode to switch to; mode selection now consults the affected-version check and logs a warning naming the Plasma version, so a portal crash during clipboard sync is explainable from the log.
- **Startup diagnostics report the linked lamco-pipewire crate version and a clean libpipewire line.** Fixes get ported to both lamco-pipewire lines, so a log alone could not tell which one a binary was built against; the build now embeds the resolved crate version and the banner logs it next to the server's own. The `pipewire --version` continuation lines were also being emitted as raw, unprefixed text with no timestamp or level; only the linked libpipewire version is reported now, on one labelled line.
- **GUI settings regrouped by feature, with a new Display tab.** The Advanced tab had grown to more than sixty settings across seven unrelated groups. Damage tracking now lives under Performance, hardware encoding under EGFX as an expert section, cursor and cursor predictor under Input, and display control plus multi-monitor on a new Display tab in the Media category. Advanced keeps video pipeline tuning, advanced video, and logging. No config fields changed, so every control keeps its existing wiring.
- **Supply-chain auditing.** A weekly `cargo audit` CI workflow runs against the lockfile and writes a readable summary to the job, failing only when vulnerabilities are present; Dependabot now covers cargo and GitHub Actions with weekly grouped minor-and-patch updates. Git-pinned forks are outside Dependabot's reach and still need manual bumps.
- **`example-config.toml` matches the current defaults again.** The example had drifted from the defaults across roughly twenty fields and still carried two keys that no longer exist, `input.use_libei` (now `input.input_protocol`) and `display.allow_rotation` (now `display.frame_transform`); the parser does not reject unknown keys, so anyone who copied the example got silently ignored settings. It also showed an IPv4-only `0.0.0.0:3389` listen address while a fresh install defaults to dual-stack `[::]:3389`. The previously undocumented `[diagnostics]` section is added, and a test now fails on a stale value or a renamed key. The file is not installed by any package.

### Fixed

- **A GNOME server could stop accepting connections after a client connect.** The EIS handshake that Mutter Direct runs for every new client used reis's tokio event stream, whose poll returns pending right after clearing readiness without polling the socket again, so no waker was registered. tokio keeps stale readiness on purpose, which makes that sequence routine, and about one activation in fifty then slept forever with the compositor's handshake reply unread on the socket. The handshake runs on the accept task, so every later client sat in the listen backlog until the server was restarted. The server now drives the EI socket with its own stream that re-polls readiness after every drain, and the handshake is bounded by a five-second timeout with one retry on a fresh ConnectToEIS. The same stream serves the Portal RemoteDesktop (libei) path. Present since EIS input was introduced; first caught by the 1.4.5 release tests.
- **The first connection after an audio-enabled client disconnects no longer fails.** IronRDP's server event channel outlives a connection, so an RDPSND wave the previous client's sound handler had already queued became the first event the next connection dispatched, before its own audio channel had negotiated a format. IronRDP treated that as a fatal PDU error and dropped the new client about a millisecond into its session. The fork (b6be611c) now drops waves that arrive before the channel is ready instead of failing the connection. New in 1.4.5, since 1.4.4 never sent waves.
- **The Community Edition GUI can start the server inside the Flatpak and Snap sandboxes.** The GUI launches the server with the D-Bus management interface enabled so it can reconnect to it, and the server refused to run at all when it could not own `io.lamco.RdpServer` on the session bus, which is exactly what both sandboxes deny by default: Snap's AppArmor policy rejected the bind and Flatpak's D-Bus proxy answered "service unknown", so Start Server in the sandboxed GUI produced a dead child and no listener, since at least 1.4.4. The Snap now declares a session-bus `dbus` slot for the name and the Flatpak manifest grants `--own-name`, so the interface works in both, and a refused registration is now a warning rather than a fatal error, so the server keeps running without the interface wherever a sandbox still says no.
- **Desktop audio now reaches the client, at the right pitch and in sync.** The RDPSND handler negotiated a format and constructed an encoder, but nothing ever started PipeWire capture, in any release to date; `audio.enabled` defaults to on, so the client negotiated sound and then heard nothing. Capture now starts with the session, PCM is chunked at `audio.frame_ms`, encoded with the negotiated codec, and delivered as RDPSND waves, and the capture stops with the session so the PipeWire thread does not leak across reconnects. The capture target is the default sink's monitor rather than the ScreenCast video node, which is not an audio node. Two follow-on faults surfaced once audio flowed. mstsc picks the first PCM format it accepts and plays it on its native 44.1 kHz endpoint without resampling, so advertising 48 kHz first dropped pitch about 1.5 semitones and let audio fall behind video by roughly five seconds a minute; 44.1 kHz is now offered first and the whole path runs at the client's rate. And waves were stamped from a per-frame accumulator that lagged whenever capture fell behind real time, so the RDPSND server discarded them as stale (audible as dropouts); each wave is now stamped from wall-clock time since capture started.
- **The server no longer crashes when a client disconnects.** On GNOME with Mutter Direct (reproduced on Fedora 44, GNOME 49.6) the server took a SIGSEGV in the video capture thread on the very first client disconnect, every time. lamco-pipewire called `pipewire::init()` and `deinit()` independently from three uncoordinated threads, and `deinit()` frees process-global library state, so whichever thread stopped first freed memory a still-running sibling was built on. A second, distinct crash then surfaced on COSMIC with audio-only PipeWire use: `pw_deinit()` itself reliably segfaults a PipeWire-internal worker thread even with exactly one user in the process. lamco-pipewire 0.6.9 and later never call the real deinit at all.
- **Blurry, laggy video on GNOME when Mutter over-reports damage.** A user-reported blurry and laggy session traced to Mutter's ScreenCast damage hints claiming 91 to 97 percent of the frame had changed on nearly every frame, while the server's own pixel-diff calibration probe measured true change at 0 to 0.2 percent, for the whole connection. The server trusted the hints, re-encoded almost the entire frame every time, and spread its fixed bitrate over pixels that had not moved. The probe is now load-bearing: after `damage_tracking.compositor_hint_distrust_consecutive_samples` (default 3) consecutive samples diverge by more than `damage_tracking.compositor_hint_distrust_threshold_pp` (default 15 percentage points), the server stops trusting that connection's compositor hints and uses its own SIMD pixel-diff detector for the rest of the connection, reporting an informational `CompositorDamageHintsDistrusted` health event. The decision is sticky per connection and each new connection starts trusted again, so a compositor fixed upstream regains the fast path automatically; compositors whose hints are accurate never cross the threshold. The over-report reproduces on Mutter 50.1, which already contains the fixes for the known upstream damage bugs, so it appears to be a distinct case, most likely specific to virtual-monitor plus llvmpipe capture. The startup banner also now reports EGFX (H.264 AVC420/AVC444) with RemoteFX fallback instead of unconditionally printing "Codec: RemoteFX".
- **A configured listen port no longer resets to 3389 on every launch (#63).** `--port` had a default of 3389, so the argument was always populated and applied unconditionally, silently overwriting whatever port `server.listen_addr` in `config.toml` specified, including a port saved from the GUI. `--listen` and `--port` (and `LAMCO_RDP_PORT`) are now applied only when explicitly given: `--listen` alone keeps the configured port, `--port` alone keeps the configured host, and neither leaves the file authoritative.
- **Clients that never negotiate EGFX get their first picture, and a reconnect burst no longer wedges the listener (#57).** When a client connected, the display loop replayed the last cached frame so there was something to send before live capture produced one, but the replay was gated on AVC support, added to suppress redundant replays during a supposed codec-negotiation window that does not exist. For a RemoteFX or V8 client that gate was permanently false, so on a desktop idle enough that damage-driven capture produced nothing within the ten-second zero-frame window, the session was declared dead before a frame ever went out. This is the mechanism behind #57, confirmed live on Fedora GNOME under Mutter Direct across 40 or more reconnect cycles; the gate now requires only EGFX readiness, and the replay also fires once the five-second EGFX gate deadline passes without readiness, for plain screenshot and automation clients with no DVC channel at all. A follow-on bug then made that legacy replay re-fire on every loop iteration, flooding the graphics queue and starving the connection's own I/O until xfreerdp3 hit a broken pipe within about a second on Debian 13 GNOME; the init flag is now cleared correctly for bypassed clients. Separately, the accept loop serves one client at a time by design, but connection attempts that completed their TCP handshake while it was busy, and whose peer then gave up, sat in the backlog as CLOSE-WAIT sockets until the loop came round; a rapid-reconnect burst reproduced 65 leaked sockets and a full backlog, after which the server accepted nothing. After each session the dispatcher now drains and drops what queued up during it, bounded to 32 attempts per listener, and logs the count; 60 near-simultaneous attempts leave zero CLOSE-WAIT sockets. Mutter session Stop failures, previously discarded, are logged at warn so a leaking session is visible rather than invisible.
- **Video recovers from a client that stops acknowledging frames instead of freezing until disconnect.** When a client stopped acknowledging EGFX frames while frames were outstanding, whether from a decoder stall under sustained motion or a single dropped ack, the flow controller's unacked count never drained, so it held the encoder throttled indefinitely and the picture froze for the rest of the session; the configured `egfx.frame_ack_timeout` never fired because nothing consulted it. The controller now checks the age of the oldest unacked frame every pass, and once it exceeds the timeout (default 5000 ms, unchanged) drops the outstanding frames, resets throttling so the encoder resumes, and forces an IDR so the client's decoder resynchronises. A new `VideoAckStalled` health event marks video degraded with the stall duration and `VideoFrameResumed` clears it.
- **Linux-to-Windows clipboard on GNOME no longer dies after the client's first copy, and the client's own copies are served back correctly.** Announcing the remote's formats makes the server the selection owner, and Mutter refuses to let an owner read its own selection back ("Tried to read own selection"); ownership persists until a local application copies, so every paste for the rest of the session failed. The server now tracks that ownership and answers those requests from its cached copy of exactly what the remote gave it, clearing the flag only once a Linux-side announcement is judged a genuine local copy, because Mutter echoes the server's own SetSelection within a millisecond. Two gaps in the same mechanism were closed during the release pass: the plain-text eager-fetch path used with the data-control providers never wrote the cache, so on Budgie an immediate read-back after a copy always missed; and the request path consulted the cache only while the ownership flag read true, which has its own races, so a paste that arrived alongside an ownership change hit Mutter's refusal on Arch and Fedora GNOME. The cache is now checked first for any format it holds, since it only ever contains this session's own RDP copies. Rapid copies were also being collapsed (copying "a", "b" then "c" left the Linux side serving "b") because a repeated format list was read as an echo by a loop check that could never have caught a real loop; every FormatList from the client is now honoured, while the echo-protection window in the other direction is untouched. On Mutter, a client's disconnect could race the next client's connect 6 ms later, so releasing the old session tore down the stream the new one had just built; establish and release now share a lifecycle lock.
- **Mouse button presses land where the client pressed, and middle-button, horizontal wheel and side buttons are decoded correctly.** IronRDP's mouse button events carried no position, so a press with no preceding move, which is exactly what a touch tap from a tablet client looks like, was injected at whatever stale position the last real move left behind. Button events now carry their own position (fixed upstream in IronRDP and inherited by the pin), and the same conversion layer had never checked the middle-button or horizontal-wheel flags and mapped the X1 and X2 side buttons to left and right. A button event now repositions the pointer before the click, horizontal scroll is injected as a discrete axis event, X1 and X2 map to `BTN_SIDE` and `BTN_EXTRA`, and relative-mode clients (whose button PDUs report the accumulated position a relative delta was applied at) also reposition before the click rather than clicking wherever the compositor's pointer happened to be. Any mouse event variant the handler does not know is logged and ignored rather than failing.
- **Input on GNOME no longer dies in an EIS reconnect storm when one device is slow to appear.** On the Mutter Direct EIS path, the injection macro treated every failure as a dead socket, including the benign case where one device type had not yet appeared in the compositor's device-setup burst. Its answer was a full reconnect, which discards every device rather than only the missing one, so with a real user driving mouse and keyboard together the next event had a good chance of racing the fresh burst and failing the same way: a self-amplifying storm that left input dead for the rest of the session, reproduced on RHEL 10 with 93 full reconnects in eight minutes. Device-readiness failures now carry their own error type and the macro waits up to two seconds for that one device before falling back to a full reconnect; any other error still reconnects immediately. Confirmed fixed on the same VM.
- **EIS input reliability.** Frame timestamps now use CLOCK_MONOTONIC, the clock libei specifies, instead of wall-clock time that can jump backwards on an NTP step. `ei_device.ready()` is sent for v3 devices (libei 1.6 and later), without which every injected event is discarded silently; it had worked only because every compositor tested so far negotiated v2. The absolute-pointer offset now comes from the captured stream's position rather than a region heuristic that matched KDE's layout but misplaced the cursor on single-output desktops and wlroots multi-output layouts. EIS device setup drains on a deterministic `sync()` barrier instead of a 500 ms quiet-timeout heuristic that could cut a slow compositor off mid-burst and proceed with zero devices, the suspected cause of a GNOME 46 zero-events failure, with a 3 s failsafe that warns. Button and scroll events from one 10 ms input batch now reach the compositor as a single EIS frame rather than one frame per event. A session also reports which pointer backend it settled on (`wlr-virtual-pointer`, `uinput` or `none`) through an informational `InputBackendSelected` health event, the `/dev/uinput` fallback now tells a missing kernel module apart from a permissions problem, and Mutter Direct's D-Bus fallback path compiles again without the `libei` feature.
- **libei video persistence on KDE (#51), and libei sessions on newer KDE no longer drop to view-only.** libei previously acquired video through a second standalone Portal ScreenCast session that hardcoded no persistence, so the screen-sharing grant re-prompted on every start on KDE, and on an unattended host `Start()` blocked indefinitely on a dialog nobody would click. Video now runs on the strategy's own RemoteDesktop session: one session, one dialog, and video persists under the same restore token as input; the input-plus-video coupling is a portal constraint, not a design choice. A redundant persist mode on the attached ScreenCast, which xdg-desktop-portal-kde 6.6.3 tolerated, is rejected outright by 6.6.4 ("Remote desktop sessions cannot persist"), which failed the whole strategy and left the session view-only with no input; persistence is now set once at the RemoteDesktop level and the attached ScreenCast inherits it. The strategy also logs what the portal `Start()` call is waiting on, since without a restore token it blocks on a permission dialog before the RDP listener binds.
- **Hyper-V Enhanced Session Mode connectivity (#52).** The vsock transport added in 1.4.4 could not accept a real Enhanced Session (VMConnect) client, which speaks plaintext Standard RDP Security over the hypervisor-isolated vsock transport and never performs a TLS handshake, so the TLS acceptor received raw RDP bytes and failed with "received corrupt message of type InvalidContentType". A new `security_mode = "rdp"` (alias `"none"`) advertises `PROTOCOL_RDP` with no TLS upgrade, and the startup security label names the mode. Because security is global to the one server, plaintext is opt-in and never auto-selected; the deployment layer warns at startup when it coincides with a TCP listener bound to a routable address, and conversely when a vsock listener is active without it. The Hyper-V use case still needs the `vsock` Cargo feature from 1.4.4.
- **Stale pixels after a resolution change on the DMA-BUF capture path (lamco-pipewire 0.6.12).** The capture crate's DMA-BUF mmap cache is keyed by file descriptor and had no per-buffer eviction. An mmap outlives the descriptor that created it and the kernel reuses low descriptor numbers, so once PipeWire destroyed a buffer generation, which it does on every format renegotiation, a descriptor number from the new generation could hit the old cache entry and be served the previous generation's pixels at the previous generation's size. Nothing failed when this happened. A client-initiated resize is the ordinary way to reach it. The capture crate now unmaps those entries on PipeWire's `remove_buffer` notification and rejects a cached mapping too small for the read being asked of it, and warns, rate-limited, when it has to take a CPU copy of a block the producer did not mark mappable, which is how a mapped GPU buffer reads as zeros without any call failing.
- **Corrupted frames are dropped, and a dead Mutter session is no longer reused.** The display loop encoded frames the producer had flagged corrupted, whose pixels and damage metadata are both untrustworthy, so encoding one put garbage on the wire; such frames are now dropped and the ordinary frame-gap handling covers the rest. Mutter Direct reused a session flagged valid without asking the compositor whether it still existed; no Closed signal arrives when the compositor's bus name vanishes outright (a shell still settling at startup, or a shell restart), so video came back on a fresh stream while input went to the dead one: picture, no keyboard. The reuse path now reads the session's `SessionId` property as a liveness check and re-establishes when that fails. A `ConnectToEIS` failure now reports `EisStreamEnded` to health instead of leaving it blind to an input channel that never came up, and after a Mutter session is torn down, queued input batches are discarded in one shot rather than refused one event at a time (#69).
- **HTML pasted from Windows on the request-driven clipboard path is decoded as UTF-8.** The path that answers a provider's on-demand request (Portal Clipboard, Mutter and wl-clipboard providers alike) treated `text/html` the same as `text/plain`, decoding it as UTF-16LE. CF_HTML is UTF-8 with an ASCII offset header, so every HTML paste through that path arrived as mojibake. It now goes through the same CF_HTML converter the data-control path uses, with line-ending sanitization for Linux.
- **One-pixel-thin screen updates are no longer discarded.** EGFX damage rectangles use inclusive right and bottom coordinates, so a region one pixel tall or wide collapsed to a degenerate rect after conversion, which the sender dropped to avoid a rectangle FreeRDP and mstsc reject with ERROR_INVALID_DATA. That discarded real updates: scrolling text rows, progress-bar lines and subtitle strips stayed stale on the client. The rectangle is now expanded by one pixel toward the display interior instead; only a strip on the very last row or column, which has nowhere to expand, is still dropped.
- **Full-frame AVC420 regions now declare the 16-aligned encoded size.** The H.264 bitstream is always padded to a multiple of 16, so at 1920x1080 the bitstream was 1920x1088 while the declared region covered only 1920x1080, which MS-RDPEGFX forbids and strict clients may reject or black-screen. Every initial connection and every periodic IDR refresh on a non-16-aligned resolution now declares the padded area.
- **Capture requests on wlroots-family compositors are paced to the configured frame rate.** On the portal-generic path, the wlr-screencopy and ext-image-copy-capture request loop re-requested the next frame the instant the previous one landed. Compositors that throttle capture to their own repaint cycle hid this, but wayfire services requests as fast as asked, so it did real render-and-copy work for frames the downstream channel had no room for; up to a 43 percent drop rate was observed on a three-core wayfire VM. The higher of `video.target_fps` and `performance.adaptive_fps.max_fps` now bounds the capture request rate (xdg-desktop-portal-generic 0.6).
- **Log file writes no longer stall capture and dispatch on a slow disk.** The file logging layer wrote synchronously from whichever thread emitted the event, so the kernel's periodic dirty-page writeback blocked the next thread to write, freezing the dispatch loop and the PipeWire capture thread together for up to 375 ms at a time at trace level. File output now goes through a non-blocking writer with a dedicated I/O thread, flushed on exit. Console-only logging is unchanged.
- **Startup names the real reason VA-API hardware encoding was not detected.** The probe shells out to `vainfo` for each render node, and when `vainfo` was not installed the spawn error read "vainfo failed for /dev/dri/renderD128: No such file", which looks like a missing device. The probe now emits one categorised line: `vainfo` (libva-utils) not installed, a VA driver present that exposes no H.264 encode entrypoint (with the Fedora `mesa-va-drivers-freeworld` hint), or no GPU render node at all; a successful detection is logged at info.
- **Jay and xfwl4 get their own compositor profiles instead of a shared Smithay guess.** The shared profile undersold Jay (omitting the `ext-image-copy-capture-v1`, virtual-keyboard and virtual-pointer protocols that the embedded portal-generic backend already treats as first-class) and oversold xfwl4 (claiming a clipboard protocol its 4.21.1 preview does not have). Each now gets its own profile, and the generic profile remains for unrecognized Smithay compositors. The niri profile no longer claims RemoteDesktop support via portal-gnome.
- **COSMIC resolves `input.input_protocol = "auto"` to libei (#65).** Today COSMIC is driven by the portal-generic branch with uinput pointer injection, selected before the preference is consulted, so nothing changes on current releases. But that branch is gated on the portal not supporting RemoteDesktop, and the day the COSMIC portal ships it, every remaining strategy is gated on the libei preference, so COSMIC would have dropped silently to view-only. COSMIC exposes no wlr-virtual-pointer and will not gain one, so EIS is the only portal-mediated input path it can get.
- **Capture health no longer reports a failure when nobody is connected.** The zero-frame detector opened a ten second window when a client connected and nothing closed it when that client left, so a client disconnecting before the first frame left the window running against a connection that no longer existed. Ten seconds later the server reported that capture had never delivered frames and drove session health to failed, with nobody watching. An idle server receiving no frames is damage-driven capture working as designed.
- **Session and clipboard robustness.** The EGFX client `queue_depth` is clamped to a plausible maximum before it feeds flow control and telemetry, and the example config's `egfx.max_frames_in_flight` is corrected from 4 to the code default of 3 (#60); stale FUSE paths from a previous server instance are ignored during Linux-to-Windows file resolution, so the client no longer sees a file it cannot fetch (#58); and the "Unknown format ID 0" clipboard warning that some KDE clients trigger is now debug-level (#59). A failed clipboard read logged a full ERROR per retry, and clients retry in bursts (64 requests about 10 ms apart on GNOME 50.4); identical failures inside a five-second window now collapse to debug with a running count. Mutter clipboard errors keep their D-Bus cause instead of reading "Failed to call SelectionRead" with the reason discarded. An idle Mutter session close after a client left is logged as a warning rather than an error, and a client that closes its socket without a Disconnect PDU (scripted clients, and a TLS peer that vanishes without close_notify) is logged at INFO rather than ERROR, with the disconnect line now walking the error's source chain so transport failures carry the detail that identifies them. D-Bus `ReloadConfig` no longer claims success when nothing was reloaded; it reports that hot reload is unavailable and the saved config takes effect on the next connection. The accept dispatcher labels desktop sessions `desktop` rather than `wlr-direct` whatever strategy is in use.
- **Accurate HDR diagnostic.** The color-space service no longer advertises best-effort HDR merely because `wp-color-management` is present; it reports HDR as unavailable with an accurate note, since the server encodes 8-bit SDR (the opt-in tone-mapping under Added is a separate path).
- **Builds against CUDA 13.3 (#56).** cudarc moves to 0.19.9, which knows the 13.3 toolkit; the NVENC path previously failed to compile with "Unsupported cuda toolkit version: 13.3".
- **Korean keyboards no longer break every Windows client (#68).** The IronRDP pin now accepts keyboard type 8 (Korean) in the client core data; before, the connection failed during finalize with "invalid keyboardType" whenever the client reported a Korean keyboard.

### Packaging

- **Minimum Rust is 1.94, and the RHEL 10 family stays at 1.4.4.** All packaging build dependencies now declare `rust >= 1.94` and `cargo >= 1.94`. RHEL 10 and AlmaLinux 10 ship rust-toolset 1.92, so RPM Fusion `el10` and OBS `AlmaLinux_10` do not build this release and their 1.4.4 packages stay in place until a RHEL 10 point release carries Rust 1.94.
- **libpipewire 0.3.62 or newer is required** at build and run time by lamco-pipewire 0.6.x, up from 0.3.33. The declared minimum for `openh264` and `openh264-sys2` is 0.9.8. IronRDP git dependencies now reference `lamco-admin/IronRDP` at upstream master `fb0c0413`, and no `[patch.crates-io]` path overrides remain; vendored source tarballs must be regenerated.
- **The systemd user unit is now `app-io.lamco.rdp-server.service`, so the portal sees a real app id (#66).** xdg-desktop-portal derives an unsandboxed process's app id from its user unit, but only for a unit whose name starts with `app-`; ours did not, so every native install presented the empty string, and portal restore-token scoping keys on that id. The unit is renamed across the native channels with `Alias=lamco-rdp-server.service` so existing `systemctl` invocations keep working, and `SystemdService=` on the D-Bus activation file. Debian's postinst, a new AUR `.install`, and `%posttrans` on both RPM specs migrate the enabled state for users logged in at upgrade time; users not logged in must re-enable by hand or the service silently stops autostarting. Snap and Flatpak are unaffected.
- **Packages enable FUSE `user_allow_other` so pasted files are readable by the file manager.** Clipboard file transfer mounts a FUSE filesystem with `allow_other`, which the kernel only permits when `user_allow_other` is set in `/etc/fuse.conf`, a root-only edit an unprivileged user service cannot make for itself. The Debian postinst and both RPM `%post` steps append it idempotently when absent, and leave it in place on removal. `fuse3` is now a hard dependency on Debian and RPM.
- **Packages recommend the `vainfo` tool the VA-API probe depends on.** Without it an install never detected VA-API and silently fell back to software encoding. Debian Recommends `vainfo`; the OBS and RPM Fusion specs Recommend `libva-utils`.
- **VA-API hardware encoding and the libopus backend on Flatpak, Snap and AUR.** The three manifests were still on the software-only `h264` tier and were missing `opus-libopus`, which is now required explicitly by any build that passes `--no-default-features`. Snap stages its own VA-API driver packages since strict confinement cannot use host drivers; AUR moves `libva` from optdepends to depends. AUR also now builds the portal-generic strategy, which it had never picked up as the default feature set grew; without it, niri fell through to a standalone Portal ScreenCast session whose linear-only DMA-BUF offer NVIDIA's tiled-only producer cannot satisfy, giving a black screen, the path both #64 reporters hit.
- **RPM Fusion, Snap and Flatpak build fixes.** RPM Fusion builds no longer run out of memory: Fedora's `%build` exports codegen flags that override the Cargo profile, so the roughly 900-crate GUI binary was SIGKILLed on Koji builders, always on ppc64le; the spec now uses line-tables debuginfo on every architecture and on ppc64le drops debuginfo, splits codegen, and turns LTO off, and EL and ppc64le builds ship no debuginfo or debugsource subpackage since line-tables debuginfo yields an empty source list EL treats as fatal. Snap gains an arm64 platform built on Launchpad through remote-build, with thin LTO and four codegen units to fit the workers' memory. The Flatpak manifest pinned the checksum of the first 1.4.4 tarball cut rather than the re-cut carrying the aarch64 fix, so building from the manifest failed; it now pins the published SHA.

## [1.4.4] - 2026-07-04



### Added

**Unified multi-transport accept layer.** A `Listener` trait and `AcceptDispatcher` abstract protocol and transport binding behind a uniform interface.

- **AF_VSOCK transport** for Hyper-V Enhanced Session Mode (closes #52). Tri-state config (`auto`, `enabled`, `disabled`) with Hyper-V DMI auto-detection. Activated via the `vsock` Cargo feature.
- **WebSocket and RDCleanPath transport** for browser and WASM clients, removing the separate ws-rdp-proxy from production deployments. Activated via the `websocket` Cargo feature. This transport ships experimental in 1.4.4 (not yet exercised end to end against the WASM client).
- **LISTEN_FDS multi-fd socket activation:** systemd can pass TCP, Unix, vsock, and WebSocket file descriptors together to one binary; descriptors are dispatched by socket name with positional fallback.

**Vulkan Video encoder.** Cross-vendor H.264 encoding via `VK_KHR_video_encode_h264`, working on NVIDIA, Intel, and AMD GPUs with Vulkan Video driver support. Activated via the `vulkan-video` Cargo feature, inside the `hardware-encoding` umbrella alongside VA-API and NVENC.

**HTTP metrics server.** Prometheus `/metrics` and JSON `/health` endpoints on a lightweight tiny-http server. Activated via the `metrics-server` Cargo feature.

**Session health sensor framework.**

- A `HealthSensor` trait with concrete sensors for PipeWire, Portal, Mutter, EGFX, and the active encoder backend.
- A `SensorRegistry` for per-session ownership of active sensors, plus a snapshot collector for periodic state capture.
- Closed-loop signaling: damage detection feeds encoding decisions, and compositor-crash detection cascades through the session lifecycle. MutterSensor and wlr-direct liveness probes, a portal health bridge, and input and clipboard health events all feed the same channel.
- The GUI Status and Performance tabs surface health output, along with live damage-source, FPS, and activity telemetry, for real-time observability.

**Linux-to-Windows clipboard file copy** on native installs, via `initiate_file_copy` with a live RDP sender wired into the FUSE read handler.

**Unicode keyboard input.** XKB keysym mapping for non-ASCII characters.

**COSMIC pointer injection** via `/dev/uinput` as an interim path while the COSMIC input portal matures.

**PAM environment self-check.** At startup the server reads the kernel `no_new_privs` bit from `/proc/self/status`; when it is set, PAM authentication is advertised as unavailable (the sgid-shadow `unix_chkpwd` helper cannot elevate, so every login would fail) and the server falls back loudly to the recommended auth method.

**OpenH264 licensing surface.** A `--licenses` flag prints the third-party license notices, reproducing Cisco's OpenH264 binary license in full. The license text also ships in the package, and in the Flatpak under `/app/share/licenses`. Cisco's attribution now appears in the EGFX codec settings, where H.264 is configured.

### Changed

- **Session lifecycle is now per-connection on GNOME/Mutter.** A `SessionLifecyclePolicy` abstraction (`Persistent` versus `PerConnection`) replaces the single long-lived RemoteDesktop and ScreenCast session. Mutter Direct now creates a fresh, dialog-free session on each connect and releases it on disconnect, so the server serves many sequential RDP sessions without a restart. Previously Mutter's idle timeout reaped the one session about eight seconds after a client left, with no rebuild path.
- **License: BUSL-1.1 revision.** The Licensor is now Lamco Development LLC following LLC formation. The Additional Use Grant is a single conditional grant with four qualifying classes (non-profit, single server instance, non-commercial education or research, and Community Edition), and the Change Date is 2029-06-01, on which this version converts to Apache-2.0.
- **lamco-pipewire de-bundled to crates.io 0.4.4** (was a bundled vendored copy); 0.4.4 carries the DMA-BUF negotiation fix for the zero-data root cause. `xdg-desktop-portal-generic` likewise de-bundled to crates.io 0.4.0.
- **MSRV is 1.89** (was 1.88), on Rust edition 2024.
- **IronRDP fork relocated** from a personal account to the organization-owned `lamco-admin/IronRDP`, and curated-rebased onto upstream master. The accept path migrated to `run_connection_with(stream, TransportTls::AlreadyDone)`, and `RdpsndServerHandler` split into `choose_format` plus `start` to match the rebased fork.
- **Dependency upgrades:** ashpd 0.13.7 (portal notification, background, and secret features), cudarc 0.19 for NVENC, vk-video 0.3.1 (encoder construction adapted to the new API), and the pipewire stack advanced accordingly.
- **ZGFX compression default remains `never`.** An interim change to `always` was reverted: the LZ77 variant degrades to O(N squared) on already-compressed H.264 payloads and stalls the EGFX pipeline on large IDR frames.
- **EGFX gate timer** is now per-connection (was per-server-uptime, which broke multi-connection scenarios), with a gate timeout for clients that never open a DVC channel.
- **libei is the default input** protocol, with touch and relative-pointer support.
- **VA-API hardened:** reference-frame management, rate control, triple-buffered output, and SIMD NV12 conversion.
- **`gui` is now a default Cargo feature.** A plain `cargo build --release` produces both `lamco-rdp-server` and `lamco-rdp-server-gui`; packaging that already opted into `gui` (Debian, RPM Fusion, OBS, Flatpak) is unchanged. Headless, server-only builds must now pass `--no-default-features` and re-enable what they need.
- **OpenH264 on Flatpak reflects the freedesktop extension retirement.** The `org.freedesktop.Platform.openh264` extension is retired and absent on the 25.08 runtime, and `codecs-extra` (x264/ffmpeg) is not a patent-covered substitute. Software H.264 uses Cisco's binary, which the user installs separately per Cisco's license; the app does not bundle or auto-download it, and the not-found guidance now points to hardware encoding, a native package, or Cisco's release list rather than the retired extension.

### Fixed

- **PAM was unusable on the hardened system service.** The system unit's seccomp options imply `NoNewPrivileges`, which the startup self-check read as "PAM unavailable", silently downgrading `auth_method=pam` to unauthenticated in exactly the multi-user case where PAM matters. The system unit now joins the `shadow` group so `pam_unix` reads `/etc/shadow` directly (no sgid `unix_chkpwd` helper), keeping the full seccomp hardening, and the self-check recognizes the direct-read path.
- **Packaged systemd units SIGSYS-killed the server at startup and made PAM authentication impossible.** Both units dropped `NoNewPrivileges=yes` and the `SystemCallFilter=~@privileged @resources` deny: libpipewire's realtime scheduling setup (`sched_setscheduler`, in `@resources`) crashed the server during PipeWire init, the `@privileged` deny killed pam_unix's in-process `setuid`, and `NoNewPrivileges` blocked the sgid-shadow `unix_chkpwd` helper and the setuid `fusermount3` from elevating, so every PAM login failed. The `@system-service` allowlist, `ProtectSystem=strict`, and `RestrictRealtime=yes` remain in place.
- **GNOME served only one RDP session before needing a restart.** Root-caused to the persistent-session model colliding with Mutter's idle-timeout reaping, and fixed by the per-connection lifecycle rework (see Changed) plus its follow-on races: capture-node rebinding is now keyed on session re-establishment rather than PipeWire node-id equality (a reused node id could bind a dead stream and freeze video); the pipeline no longer pauses on unserved or probe disconnects; EGFX shared handler state resets on each new connection; and Mutter clipboard signal listeners re-subscribe after re-establishment.
- **GNOME 49 input** stopped working until a lazy Mutter EIS lifecycle with a correct keep-alive connection was added (closes #45). The EIS capability bind mask is now accumulated from advertised `Seat::Capability` events instead of a blanket union.
- **KDE Windows-to-Linux clipboard file transfer** was broken because Klipper re-announced a file URI as `text/plain` and the cooperation handler forwarded that re-announcement, discarding the portal's block decision; `SendInitiateCopy` is now gated on the portal sync decision. The persistent clipboard monitor also binds `ext-data-control-v1` (which KWin exposes, not `wlr-data-control-v1`), and Klipper empty-selection clearing is gated to KDE.
- **sway and wlroots color skew** (blue rendered as brown): the portal-generic direct-frame bridge defaulted every frame to BGRx, but sway delivers `Xbgr8888` (RGBx byte order) via wlr-screencopy. The bridge now honors the `wl_shm` capture format and swaps the R and B channels in place.
- **COSMIC delivered zero frames** compared with 1.4.2; capture is now routed to the portal-generic embedded strategy via a capture-protocol gate.
- **WebSocket and RDCleanPath listener could drop a connection mid-handshake** when another transport won the shared accept race. The handshake now runs in a background task off the cancellable accept path.
- **SIMD damage detection** corrected on aarch64 (NEON) and AVX2; the AVX2 kernel is now runtime-dispatched (`is_x86_feature_detected!`) so stock baseline-x86_64 packages use it instead of silently falling back to scalar.
- **GUI fixes:** 38 text-input fields that silently discarded typed text now write back to the fields iced renders from; notification messages render; file dialogs degrade gracefully when no FileChooser portal is present; startup skips the wgpu hardware probe on virtual GPUs; `LAMCO_GUI_SOFTWARE` is honored before the GPU probe; "Stop Server" now stops a D-Bus-connected server; and log-directory writability is validated with pre-init failures classified correctly.
- **GUI failed to detect externally-started servers.** The headless server now writes its PID to `$XDG_RUNTIME_DIR/lamco-rdp-server.pid` (the path the GUI's `check_pid_file()` has always expected) at startup and removes it on shutdown, so a server started via systemd, SSH, or any other launcher is now visible to the GUI.
- **TLS material now self-heals** at startup, after logging init, and Snap sandboxing is detected, fixing a confusing headless-start failure after a user config was removed.
- **Health monitoring** no longer flags an idle static desktop as a video stall (health is driven by authoritative stream-state events, not frame timing), reports an idle capture stream as healthy between clients, and gates the EGFX channel-closed warning on an active client.
- **EGFX:** V8 client support and a handler-state race (#4); uncompressed fallback, buffer-tier selection, and virgl flip; `gfx_server_handle` preserved in `updates()` with retry under contention; `egfx_needs_init` reset on reconnection; OpenH264 `CONSTANT_ID` corrected from 1 to 0; SPS/PPS prepending removed on P-frames; and degenerate metablock rects rejected so strict RDP clients no longer disconnect.
- **Clipboard:** Mutter D-Bus `as` (array of strings) MIME parsing, double-request caching, poll-based FD reads, and FD close after `selection_write_done`; FUSE access/getxattr/listxattr operations implemented; all pending serials answered in Windows-to-Linux file paste; stale-format and idempotent-disconnect edge cases; and `file://` URIs percent-encoded with three duplicate implementations de-duplicated.
- **EIS:** pointer-absolute device separation, a RefCell panic, event timeouts, deferred activation, `start_emulating`, the DMA-BUF-to-MemFd fallback on virtual GPUs, a keycode offset, and capture-device regions for coordinate mapping.
- **Portal input injection** recovers when the ScreenCast stream pauses (#30); virtual GPUs are detected and forced to MemFd over DMA-BUF, which was returning zero data (#43, #47).
- **Protocol and compatibility:** IPv6 dual-stack listen; FreeRDP 3.x TLS negotiation (#40); `auth_method=none` no longer rejects clients that send credentials (#35); FUSE clipboard stale-mount cleanup on restart (#46); Hyper-V Enhanced Session Mode via AF_VSOCK (#52); and `cliprdr` file transfer aligned with the new auto Lock/Unlock contract.

### Security

- **Startup exposure warning** when `auth_method=none` is combined with a non-loopback `listen_addr`, i.e. an unauthenticated RDP listener reachable on the network. The Portal still gates screen capture interactively, so this warns rather than refuses.
- `unsafe_code` lints migrated to `#[expect(reason = "...")]` with documented justification.
- Eliminated undefined behavior in `server_process`.

---

## [1.4.2] - 2026-03-10

### Added

**DirectChannel Capture and Protocol Routing** — new capture topology option for direct channel deployments.

**PAM Authentication**

- PAM auth method with rate limiting.
- PAM service configuration files for distribution packaging.
- Auto-disabled in Flatpak sandbox.

**Config Loading Overhaul**

- figment-based config loading with `--generate-config` flag.

**Compositor-Aware Input Protocol Auto-Detection** — picks libei vs wlr based on detected compositor.

**PTS-Based Frame Timing** in display pipeline.

**libei Default Input** — touch and relative-pointer support; extracted `eis_common` module.

**VA-API Hardening**

- Reference frame management, rate control, triple-buffered output, SIMD NV12 conversion.

**Video Frame Stall Detection** in the health subsystem.

**EGFX Improvements**

- Gate timeout for clients that don't open a DVC channel.
- Per-connection gate timer (was per-server-uptime).
- Bitmap fallback for V8 EGFX clients without AVC support.

**Clipboard Eager-Fetch (Option D Hybrid)** for data-control providers.

### Changed

- **Dependency upgrades**: zbus 5, pipewire 0.9, lamco-pipewire 0.3 (bundled with DMA-BUF zero-data detection), rcgen 0.14 (with x509-parser and zeroize), xkbcommon 0.9, criterion 0.8, rfd 0.17, sysinfo 0.38.
- **xdg-desktop-portal-generic 0.2.0** support.
- **Removed** mockall dependency.
- **Audio**: PCM 44100Hz fallback for broader client compatibility.

### Fixed

- **#34** — EGFX display pipeline drops all frames for V8 clients (bitmap fallback added).
- **#36** — Server 100% CPU at idle from spin loops (sleep added to bare-continue paths).
- **#37** — Server silently fails to capture when `WAYLAND_DISPLAY` unset (warning added).
- **#39** — FUSE clipboard mount failure message improved with common causes.
- **PortalGeneric** Drop panic.
- **BitmapConverter** resets on client reconnection.
- **`egfx_needs_init`** cleared for all clients, not just AVC.
- **Standalone ScreenCast acquisition** extended to libei strategy.
- **Mutter clipboard**: `SelectionOwnerChanged` tuple-wrapped MIME parsing; prevent empty `FormatList` from stealing Wayland clipboard ownership; skip duplicate `EnableClipboard`; provide eager-fetched data under both bare and charset MIME keys.
- **Display**: defer resize to actual PipeWire negotiated resolution; ignore resize in direct channel mode.
- **Resize degradation**, audio teardown race, systemd sandbox interaction.
- **`auth_method=none`** path: pass `None` credentials.
- **Accept loop** exits only when Portal session is destroyed.
- **Clipboard manager and portal backend detection** improvements.
- **Unicode keyboard events** mapped to evdev keycodes (Bug-O foundation).

---

## [1.4.1] - 2026-03-03

### Changed

- Packaging maintenance release. Distribution channel fixes: cros-libva quilt patch refresh, systemd hardening, and libfuse3 runtime dependency. No application code changes from 1.4.0.

---

## [1.4.0] - 2026-02-24

### Added

**Session Health Monitoring**
- Real-time health monitor tracking PipeWire stream state, Portal session validity, and EIS input streams
- D-Bus signal relay on `io.lamco.rdp_server.Health` for external monitoring tools
- Health states: Healthy, Degraded (with reasons), Failed
- Compositor crash detection and automatic recovery transitions
- SubsystemNotAvailable state for absent subsystems

**Clipboard Provider Architecture**
- ext-data-control-v1 and wlr-data-control-v1 as first-class clipboard backends
- WlClipboardProvider for wlroots compositors (tested on Hyprland via AUR)
- Persistent data-control monitor with text MIME aliases
- Automatic upgrade from Portal clipboard to data-control when detected in native mode
- portal-generic strategy with clipboard wiring for wlr-direct sessions

**View-Only Mode**
- ScreenCast-only strategy for monitoring without input injection
- `--view-only` flag or GUI toggle
- Health reporter integration for ScreenCastOnly sessions

**OpenH264 Dynamic Loading**
- Runtime loading of Cisco's pre-built OpenH264 binary (patent-compliant)
- Dual ABI support: ABI 7 (OpenH264 2.3.x) and ABI 8 (OpenH264 2.5.x)
- Graceful degradation when no codec found (skip EGFX surface creation)

**Community Edition**
- Flatpak and Snap distributions designated as Community Edition (free to use)
- Full sandbox philosophy: Portal-only APIs, no escape hatches
- Snap build with GUI and libei features

**D-Bus Management Interface**
- `session_type` property exposed via D-Bus
- Stopped status emitted on shutdown

### Changed

- MSRV raised to 1.88 (iced 0.14, edition 2024 features)
- SIGTERM handler for graceful shutdown alongside existing SIGINT
- wlr-direct pointer coordinate handling improved
- Cursor mode selection: best available mode for wlr-direct ScreenCast
- Real Wayland global enumeration with strategy fallback
- Portal session validity guards on clipboard and input paths

### Fixed

- AVC444 aux stream SPS/PPS no longer stripped (fixes chroma rendering)
- Health reporter ordering corrected (SessionInvalidated cascade)
- MutterDirect closed listeners no longer capture prematurely
- Portal session invalidity correctly propagates to clipboard subsystem
- 324 clippy warnings resolved across codebase
- 10 test-build warnings resolved
- Compiler warnings in wlr-direct and selector modules suppressed

---

## [1.3.1] - 2026-02-13

### Changed

- Flathub packaging: desktop file, MetaInfo, and icons ship with source tarball
- Clippy pedantic linting pass (deny-level pedantic warnings)
- iced 0.14 downgraded to 0.13 for OBS distro Rust compatibility
- Codebase reformatted with new rustfmt and editorconfig configuration
- Portal protocol compliance audit and roadmap documented
- OBS build procedure and dependency version constraints documented

---

## [1.3.0] - 2026-02-07

### Added

- KDE Klipper clipboard cooperation mode via direct D-Bus integration
- Session factory with automatic platform quirk detection
- KDE Portal Clipboard threading bug detection (Bug 515465, versions 6.3.90-6.5.5)
- Async task shutdown for clipboard processor and event streams
- Portal session and clipboard cleanup on disconnect

### Fixed

- EGFX black screen on reconnection (damage detection state not reset)
- Clipboard actually disabled on KDE when quirk detected (was being ignored)
- Clipboard cleared on reconnect, not just shutdown
- Input handler state reset on client reconnection
- Graceful log file creation fallback for sandboxed environments
- Explicit PipeWire shutdown for clean session teardown
- Rdp server run() raced against shutdown broadcast to prevent Quit event consumption

### Changed

- GUI reorganized with wired settings and server detach mode
- Clipboard naming standardized (Manager to Orchestrator)
- Module headers added with execution path documentation

---

## [1.2.2] - 2026-01-21

### Added

- D-Bus management interface for GUI-server IPC
- GUI D-Bus client for server management
- Authentication services in service registry
- Dynamic auth methods from service registry in GUI
- Flatpak support in GUI with multi-monitor tab
- SessionFactory pattern with compositor-specific session creation
- Portal clipboard working in Flatpak on GNOME 46

### Fixed

- GUI startup on systems without GPU passthrough (software rendering fallback)
- PAM auth auto-disabled in Flatpak sandbox environment
- Clipboard manager passed to Portal session creation
- App ID corrected to io.lamco.rdp-server for Flatpak

### Changed

- lamco-portal moved to published crate (v0.3.1) from bundled dependency

---

## [1.0.0] - 2026-01-19

### Added

**Full-Featured Configuration GUI**
- Professional dark theme with Lamco branding
- 10-tab interface covering all 85+ configuration parameters:
  - Server: Listen address, connections, timeouts, XDG portals
  - Security: TLS certificates, authentication, NLA
  - Video: Codec selection, FPS, quality, latency modes
  - Input: Keyboard layout, mouse behavior, touch support
  - Clipboard: Synchronization, rate limiting, MIME filtering
  - Logging: Log levels, output destinations, rotation
  - Performance: Buffer sizes, threading, damage detection
  - EGFX: H.264 encoding, IDR keyframes, chroma subsampling
  - Advanced: Service registry, experimental features
  - Status: Server control, live logs, system info
- TLS certificate generation wizard
- Server process management (start/stop/restart from GUI)
- Live log viewer with filtering
- Real-time configuration validation
- Import/Export configuration files
- Hardware detection and capability display

**GUI Framework**
- Built with iced 0.14 (pure Rust, cross-platform)
- Optional feature (`--features gui`) - server works without GUI
- Separate binary: `lamco-rdp-server-gui`
- Professional enterprise aesthetic (dark theme like Grafana/DataDog)

### Changed

- Version bump to 1.0.0 (stable release)

### Upgrade Notes

To run the GUI:
```bash
# Build with GUI feature
cargo build --release --features gui

# Run GUI
./target/release/lamco-rdp-server-gui
```

Or via Flatpak (when available):
```bash
flatpak run io.lamco.rdp-server-gui
```

---

## [0.9.0] - 2026-01-18

### Added

**Multi-Strategy Session Persistence**
- Mutter Direct API strategy (GNOME, zero dialogs)
- wlr-direct strategy (wlroots native protocols, zero dialogs)
- Portal + libei/EIS strategy (Flatpak-compatible wlroots)
- Portal + Restore Tokens strategy (universal, Portal v4+)
- Basic Portal fallback strategy
- Automatic strategy selection based on compositor detection
- Encrypted credential storage (Secret Service, TPM 2.0, encrypted file)

**Service Advertisement Registry**
- 18 advertised services with 4-level guarantees (Guaranteed, BestEffort, Degraded, Unavailable)
- Runtime feature detection and translation (Wayland capabilities → RDP features)
- Compositor-specific profiles (GNOME, KDE, wlroots, COSMIC)
- Service-based decision making for codec selection, FPS tuning, cursor mode

**Video & Graphics**
- H.264 video encoding via EGFX graphics pipeline
- AVC420 (4:2:0 chroma) and AVC444 (4:4:4 chroma) codec support
- Hardware-accelerated encoding (VA-API for Intel/AMD, NVENC for NVIDIA)
- Damage region detection with SIMD optimization (90%+ bandwidth savings)
- Adaptive FPS (5-60 FPS based on screen activity)
- Latency governor (interactive/balanced/quality modes)
- Periodic IDR keyframe generation for artifact clearing

**Input & Interaction**
- Complete keyboard and mouse support (200+ key mappings)
- Multi-monitor coordinate transformation
- Predictive cursor with physics-based latency compensation
- Touch input support (experimental)

**Clipboard**
- Bidirectional clipboard synchronization
- Text, image, and file transfer support
- Loop detection and prevention
- FUSE-based on-demand file transfer
- Format conversion (15+ clipboard formats)
- Rate limiting for Portal compatibility

**Deployment & Compatibility**
- Flatpak packaging with sandbox permissions
- systemd user and system service files
- RPM and DEB package specifications
- Automatic compositor detection (GNOME, KDE, wlroots, COSMIC)
- Portal capability probing
- Deployment context detection (Flatpak, systemd, native)

**Configuration & Management**
- Comprehensive TOML configuration with all options
- Environment variable support
- CLI diagnostic commands (--show-capabilities, --persistence-status, --diagnose)
- User-friendly error messages with troubleshooting hints
- TLS 1.3 with automatic or manual certificate management

### Tested Platforms

- ✅ Ubuntu 24.04 LTS (GNOME 46, Portal v5) - Full RDP functionality
- ✅ RHEL 9.7 (GNOME 40, Portal v4) - Video and input working (no clipboard)
- ⚠️ Pop!_OS 24.04 COSMIC - Limited support (Portal RemoteDesktop not implemented)

See `docs/DISTRO-TESTING-MATRIX.md` for complete compatibility matrix.

### Known Issues

**GNOME Session Persistence**
- GNOME portal backend rejects persistence for RemoteDesktop sessions (policy decision)
- Mutter Direct API strategy bypasses this limitation on GNOME 42+
- Impact: Permission dialog required on each server restart with Portal strategy

**COSMIC Desktop**
- Portal RemoteDesktop interface not implemented in COSMIC
- Waiting on Smithay PR #1388 (Ei protocol support)
- Impact: No input injection available on COSMIC in Flatpak

**Portal Clipboard (Ubuntu 24.04)**
- xdg-desktop-portal-gnome may crash on complex Excel paste operations
- Impact: Session becomes unusable after crash
- Workaround: Avoid pasting Excel with 15+ formats

### Dependencies

**Published Lamco Crates:**
- lamco-wayland 0.2.3
- lamco-rdp 0.5.0
- lamco-portal 0.3.0
- lamco-pipewire 0.1.4
- lamco-video 0.1.2
- lamco-rdp-input 0.1.1

**Bundled Crates:**
- lamco-clipboard-core 0.5.0 (local path dependency)
- lamco-rdp-clipboard 0.2.2 (local path dependency)

**Forked Dependencies:**
- IronRDP fork (github.com/lamco-admin/IronRDP)
  - Includes: MS-RDPEGFX support (PR #1057 pending upstream)
  - Clipboard file transfer methods (PRs #1063-1066 merged upstream)

### License

Business Source License 1.1 (BUSL-1.1)
- Free for non-profits and small businesses (<3 employees, <$1M revenue)
- Commercial license required for larger organizations ($49.99/year or $99 perpetual)
- Automatically converts to Apache License 2.0 on December 31, 2028

### Build Requirements

- Rust 1.77+
- PipeWire 0.3.77+
- XDG Desktop Portal
- OpenSSL (for TLS)
- Optional: libva 1.20+ (VA-API hardware encoding)
- Optional: NVIDIA driver + CUDA (NVENC hardware encoding)

### Runtime Requirements

- Linux with Wayland compositor (GNOME 42+, KDE 6+, wlroots-based)
- XDG Desktop Portal with ScreenCast and RemoteDesktop support
- PipeWire for video capture
- D-Bus session bus

---

## Versioning

lamco-rdp-server follows Semantic Versioning (semver):
- MAJOR version for incompatible API changes
- MINOR version for backwards-compatible functionality additions
- PATCH version for backwards-compatible bug fixes

**Current:** v1.4.4
**Previous:** v1.4.2, v1.4.1, v1.4.0, v1.3.1, v1.3.0, v1.2.2, v1.0.0, v0.9.0
