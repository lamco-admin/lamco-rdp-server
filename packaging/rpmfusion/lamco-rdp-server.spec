#
# spec file for package lamco-rdp-server
#
# Copyright (c) 2026 Lamco Development <office@lamco.io>
# License: BUSL-1.1
#

Name:           lamco-rdp-server
Version:        1.4.0
Release:        1%{?dist}
Summary:        Wayland RDP server for Linux desktop sharing with GUI

License:        BUSL-1.1 AND 0BSD AND Apache-2.0 AND Apache-2.0 WITH LLVM-exception AND BSD-1-Clause AND BSD-2-Clause AND BSD-3-Clause AND BSL-1.0 AND CC0-1.0 AND GPL-2.0-only AND ISC AND LGPL-2.1-or-later AND MIT AND MIT-0 AND MPL-2.0 AND OpenSSL AND Unicode-3.0 AND Unlicense AND Zlib
URL:            https://www.lamco.ai/products/lamco-rdp-server/
Source0:        https://github.com/lamco-admin/lamco-rdp-server/releases/download/v%{version}/%{name}-%{version}.tar.xz

# Disable Fedora's system-level LTO flags to prevent double-LTO interaction
# with Cargo's own LTO (we use CARGO_PROFILE_RELEASE_LTO=thin below)
%global _lto_cflags %{nil}

# Fix build with libva >= 2.22 (Fedora rawhide): new fields in VAEncPictureParameterBufferVP9
# Upstream cros-libva is dormant; this adds ..Default::default() to handle unknown fields
Patch0:         cros-libva-vp9-compat.patch

# Vendored Rust crates: 883 crates vendored in source tarball.
# Non-free package cannot use distro Rust crate packaging; cargo --offline
# builds from vendor/ with .cargo/config.toml pointing to local sources.
# License tag enumerates all licenses found in vendored crates.
# Generated from: cargo metadata + vendor/*/Cargo.toml

# Rust toolchain (MSRV 1.88: iced 0.14 requires edition 2024 features)
BuildRequires:  rust >= 1.88
BuildRequires:  cargo >= 1.88

# System libraries
BuildRequires:  pkgconfig
BuildRequires:  gcc
BuildRequires:  gcc-c++
BuildRequires:  make
BuildRequires:  cmake
BuildRequires:  nasm

# PipeWire
BuildRequires:  pkgconfig(libpipewire-0.3)
BuildRequires:  pkgconfig(libspa-0.2)

# Wayland/Portal
BuildRequires:  pkgconfig(wayland-client)
BuildRequires:  pkgconfig(xkbcommon)

# D-Bus
BuildRequires:  pkgconfig(dbus-1)

# VA-API (hardware encoding)
BuildRequires:  pkgconfig(libva) >= 1.20.0

# PAM (authentication)
BuildRequires:  pam-devel

# OpenSSL (TLS)
BuildRequires:  pkgconfig(openssl)

# FUSE (clipboard file transfer)
BuildRequires:  pkgconfig(fuse3)

# Clang for bindgen
BuildRequires:  clang
BuildRequires:  clang-devel

# Desktop/metainfo validation
BuildRequires:  desktop-file-utils
BuildRequires:  libappstream-glib

# Systemd macros for user service
BuildRequires:  systemd-rpm-macros

# Runtime dependencies
Requires:       pipewire
Requires:       xdg-desktop-portal
Requires:       pam

# Weak dependencies for hardware encoding
Recommends:     libva
Recommends:     intel-media-driver
Recommends:     mesa-va-drivers

# Bundled crate provides (883 vendored Rust crates)
Provides:       bundled(crate(ab_glyph)) = 0.2.32
Provides:       bundled(crate(ab_glyph_rasterizer)) = 0.1.10
Provides:       bundled(crate(adler2)) = 2.0.1
Provides:       bundled(crate(aead)) = 0.5.2
Provides:       bundled(crate(aead)) = 0.6.0-rc.2
Provides:       bundled(crate(aes)) = 0.8.4
Provides:       bundled(crate(aes)) = 0.9.0-rc.1
Provides:       bundled(crate(aes-gcm)) = 0.10.3
Provides:       bundled(crate(aes-gcm)) = 0.11.0-rc.1
Provides:       bundled(crate(aes-kw)) = 0.3.0-rc.1
Provides:       bundled(crate(ahash)) = 0.7.8
Provides:       bundled(crate(ahash)) = 0.8.12
Provides:       bundled(crate(aho-corasick)) = 1.1.4
Provides:       bundled(crate(aliasable)) = 0.1.3
Provides:       bundled(crate(allocator-api2)) = 0.2.21
Provides:       bundled(crate(android-activity)) = 0.6.0
Provides:       bundled(crate(android-properties)) = 0.2.2
Provides:       bundled(crate(android_system_properties)) = 0.1.5
Provides:       bundled(crate(anes)) = 0.1.6
Provides:       bundled(crate(annotate-snippets)) = 0.9.2
Provides:       bundled(crate(anstream)) = 0.6.21
Provides:       bundled(crate(anstyle)) = 1.0.13
Provides:       bundled(crate(anstyle-parse)) = 0.2.7
Provides:       bundled(crate(anstyle-query)) = 1.1.5
Provides:       bundled(crate(anstyle-wincon)) = 3.0.11
Provides:       bundled(crate(anyhow)) = 1.0.100
Provides:       bundled(crate(approx)) = 0.5.1
Provides:       bundled(crate(arrayref)) = 0.3.9
Provides:       bundled(crate(arrayvec)) = 0.7.6
Provides:       bundled(crate(ash)) = 0.37.3
Provides:       bundled(crate(ashpd)) = 0.11.1
Provides:       bundled(crate(ashpd)) = 0.12.1
Provides:       bundled(crate(asn1-rs)) = 0.7.1
Provides:       bundled(crate(asn1-rs-derive)) = 0.6.0
Provides:       bundled(crate(asn1-rs-impl)) = 0.2.0
Provides:       bundled(crate(as-raw-xcb-connection)) = 1.0.1
Provides:       bundled(crate(async-broadcast)) = 0.7.2
Provides:       bundled(crate(async-channel)) = 2.5.0
Provides:       bundled(crate(async-dnssd)) = 0.5.1
Provides:       bundled(crate(async-executor)) = 1.13.3
Provides:       bundled(crate(async-fs)) = 2.2.0
Provides:       bundled(crate(async-io)) = 2.6.0
Provides:       bundled(crate(async-lock)) = 3.4.2
Provides:       bundled(crate(async-process)) = 2.5.0
Provides:       bundled(crate(async-recursion)) = 1.1.1
Provides:       bundled(crate(async-signal)) = 0.2.13
Provides:       bundled(crate(async-task)) = 4.7.1
Provides:       bundled(crate(async-trait)) = 0.1.89
Provides:       bundled(crate(atomic-polyfill)) = 1.0.3
Provides:       bundled(crate(atomic-waker)) = 1.1.2
Provides:       bundled(crate(autocfg)) = 1.5.0
Provides:       bundled(crate(aws-lc-rs)) = 1.15.4
Provides:       bundled(crate(aws-lc-sys)) = 0.37.0
Provides:       bundled(crate(base16ct)) = 0.3.0
Provides:       bundled(crate(base16ct)) = 1.0.0
Provides:       bundled(crate(base64)) = 0.22.1
Provides:       bundled(crate(base64ct)) = 1.8.3
Provides:       bundled(crate(bindgen)) = 0.69.5
Provides:       bundled(crate(bindgen)) = 0.70.1
Provides:       bundled(crate(bit_field)) = 0.10.3
Provides:       bundled(crate(bitflags)) = 1.3.2
Provides:       bundled(crate(bitflags)) = 2.10.0
Provides:       bundled(crate(bit-set)) = 0.5.3
Provides:       bundled(crate(bit-set)) = 0.8.0
Provides:       bundled(crate(bit-vec)) = 0.6.3
Provides:       bundled(crate(bit-vec)) = 0.8.0
Provides:       bundled(crate(bitvec)) = 1.0.1
Provides:       bundled(crate(block)) = 0.1.6
Provides:       bundled(crate(block2)) = 0.5.1
Provides:       bundled(crate(block2)) = 0.6.2
Provides:       bundled(crate(block-buffer)) = 0.10.4
Provides:       bundled(crate(block-buffer)) = 0.11.0-rc.5
Provides:       bundled(crate(blocking)) = 1.6.2
Provides:       bundled(crate(block-padding)) = 0.3.3
Provides:       bundled(crate(block-padding)) = 0.4.0-rc.4
Provides:       bundled(crate(bumpalo)) = 3.19.1
Provides:       bundled(crate(by_address)) = 1.2.1
Provides:       bundled(crate(bytemuck)) = 1.24.0
Provides:       bundled(crate(bytemuck_derive)) = 1.10.2
Provides:       bundled(crate(byteorder)) = 1.5.0
Provides:       bundled(crate(byteorder-lite)) = 0.1.0
Provides:       bundled(crate(bytes)) = 1.11.0
Provides:       bundled(crate(calloop)) = 0.13.0
Provides:       bundled(crate(calloop)) = 0.14.3
Provides:       bundled(crate(calloop-wayland-source)) = 0.3.0
Provides:       bundled(crate(calloop-wayland-source)) = 0.4.1
Provides:       bundled(crate(cast)) = 0.3.0
Provides:       bundled(crate(cbc)) = 0.1.2
Provides:       bundled(crate(cbc)) = 0.2.0-rc.1
Provides:       bundled(crate(cc)) = 1.2.54
Provides:       bundled(crate(cesu8)) = 1.1.0
Provides:       bundled(crate(cexpr)) = 0.6.0
Provides:       bundled(crate(cfg_aliases)) = 0.1.1
Provides:       bundled(crate(cfg_aliases)) = 0.2.1
Provides:       bundled(crate(cfg-expr)) = 0.15.8
Provides:       bundled(crate(cfg-if)) = 1.0.4
Provides:       bundled(crate(chrono)) = 0.4.43
Provides:       bundled(crate(ciborium)) = 0.2.2
Provides:       bundled(crate(ciborium-io)) = 0.2.2
Provides:       bundled(crate(ciborium-ll)) = 0.2.2
Provides:       bundled(crate(cipher)) = 0.4.4
Provides:       bundled(crate(cipher)) = 0.5.0-rc.1
Provides:       bundled(crate(clang-sys)) = 1.8.1
Provides:       bundled(crate(clap)) = 4.5.56
Provides:       bundled(crate(clap_builder)) = 4.5.56
Provides:       bundled(crate(clap_derive)) = 4.5.55
Provides:       bundled(crate(clap_lex)) = 0.7.7
Provides:       bundled(crate(clipboard_macos)) = 0.1.1
Provides:       bundled(crate(clipboard_wayland)) = 0.2.2
Provides:       bundled(crate(clipboard-win)) = 5.4.1
Provides:       bundled(crate(clipboard_x11)) = 0.4.3
Provides:       bundled(crate(cmake)) = 0.1.57
Provides:       bundled(crate(codespan-reporting)) = 0.11.1
Provides:       bundled(crate(colorchoice)) = 1.0.4
Provides:       bundled(crate(color_quant)) = 1.1.0
Provides:       bundled(crate(com)) = 0.6.0
Provides:       bundled(crate(combine)) = 4.6.7
Provides:       bundled(crate(com_macros)) = 0.6.0
Provides:       bundled(crate(com_macros_support)) = 0.6.0
Provides:       bundled(crate(concurrent-queue)) = 2.5.0
Provides:       bundled(crate(const-oid)) = 0.10.2
Provides:       bundled(crate(const-oid)) = 0.9.6
Provides:       bundled(crate(convert_case)) = 0.6.0
Provides:       bundled(crate(cookie-factory)) = 0.3.3
Provides:       bundled(crate(core-foundation)) = 0.10.1
Provides:       bundled(crate(core-foundation)) = 0.9.4
Provides:       bundled(crate(core-foundation-sys)) = 0.8.7
Provides:       bundled(crate(core-graphics)) = 0.23.2
Provides:       bundled(crate(core-graphics-types)) = 0.1.3
Provides:       bundled(crate(cosmic-text)) = 0.12.1
Provides:       bundled(crate(cpufeatures)) = 0.2.17
Provides:       bundled(crate(crc32fast)) = 1.5.0
Provides:       bundled(crate(criterion)) = 0.5.1
Provides:       bundled(crate(criterion-plot)) = 0.5.0
Provides:       bundled(crate(critical-section)) = 1.2.0
Provides:       bundled(crate(cros-libva)) = 0.0.13
Provides:       bundled(crate(crossbeam-channel)) = 0.5.15
Provides:       bundled(crate(crossbeam-deque)) = 0.8.6
Provides:       bundled(crate(crossbeam-epoch)) = 0.9.18
Provides:       bundled(crate(crossbeam-utils)) = 0.8.21
Provides:       bundled(crate(crunchy)) = 0.2.4
Provides:       bundled(crate(crypto-bigint)) = 0.7.0-rc.8
Provides:       bundled(crate(crypto-common)) = 0.1.7
Provides:       bundled(crate(crypto-common)) = 0.2.0-rc.4
Provides:       bundled(crate(cryptoki)) = 0.10.0
Provides:       bundled(crate(cryptoki-sys)) = 0.4.0
Provides:       bundled(crate(crypto-mac)) = 0.11.0
Provides:       bundled(crate(crypto-primes)) = 0.7.0-pre.3
Provides:       bundled(crate(ctor-lite)) = 0.1.0
Provides:       bundled(crate(ctr)) = 0.10.0-rc.1
Provides:       bundled(crate(ctr)) = 0.9.2
Provides:       bundled(crate(cudarc)) = 0.16.6
Provides:       bundled(crate(cursor-icon)) = 1.2.0
Provides:       bundled(crate(curve25519-dalek)) = 5.0.0-pre.1
Provides:       bundled(crate(curve25519-dalek-derive)) = 0.1.1
Provides:       bundled(crate(d3d12)) = 0.19.0
Provides:       bundled(crate(dark-light)) = 1.1.1
Provides:       bundled(crate(data-encoding)) = 2.10.0
Provides:       bundled(crate(data-url)) = 0.3.2
Provides:       bundled(crate(dconf_rs)) = 0.3.0
Provides:       bundled(crate(der)) = 0.7.10
Provides:       bundled(crate(der)) = 0.8.0-rc.9
Provides:       bundled(crate(deranged)) = 0.5.5
Provides:       bundled(crate(der_derive)) = 0.7.3
Provides:       bundled(crate(der-parser)) = 10.0.0
Provides:       bundled(crate(des)) = 0.9.0-rc.1
Provides:       bundled(crate(detect-desktop-environment)) = 0.2.0
Provides:       bundled(crate(digest)) = 0.10.7
Provides:       bundled(crate(digest)) = 0.11.0-rc.3
Provides:       bundled(crate(dirs)) = 4.0.0
Provides:       bundled(crate(dirs)) = 5.0.1
Provides:       bundled(crate(dirs-sys)) = 0.3.7
Provides:       bundled(crate(dirs-sys)) = 0.4.1
Provides:       bundled(crate(dispatch)) = 0.2.0
Provides:       bundled(crate(dispatch2)) = 0.3.0
Provides:       bundled(crate(displaydoc)) = 0.2.5
Provides:       bundled(crate(dlib)) = 0.5.2
Provides:       bundled(crate(dlv-list)) = 0.3.0
Provides:       bundled(crate(downcast)) = 0.11.0
Provides:       bundled(crate(downcast-rs)) = 1.2.1
Provides:       bundled(crate(dpi)) = 0.1.2
Provides:       bundled(crate(drm)) = 0.14.1
Provides:       bundled(crate(drm-ffi)) = 0.9.0
Provides:       bundled(crate(drm-fourcc)) = 2.2.0
Provides:       bundled(crate(drm-sys)) = 0.8.0
Provides:       bundled(crate(dunce)) = 1.0.5
Provides:       bundled(crate(ecdsa)) = 0.17.0-rc.7
Provides:       bundled(crate(ed25519)) = 3.0.0-rc.1
Provides:       bundled(crate(ed25519-dalek)) = 3.0.0-pre.1
Provides:       bundled(crate(either)) = 1.15.0
Provides:       bundled(crate(elliptic-curve)) = 0.14.0-rc.15
Provides:       bundled(crate(endi)) = 1.1.1
Provides:       bundled(crate(enum-as-inner)) = 0.6.1
Provides:       bundled(crate(enumflags2)) = 0.7.12
Provides:       bundled(crate(enumflags2_derive)) = 0.7.12
Provides:       bundled(crate(equivalent)) = 1.0.2
Provides:       bundled(crate(errno)) = 0.3.14
Provides:       bundled(crate(error-code)) = 3.3.2
Provides:       bundled(crate(etagere)) = 0.2.15
Provides:       bundled(crate(euclid)) = 0.22.13
Provides:       bundled(crate(event-listener)) = 5.4.1
Provides:       bundled(crate(event-listener-strategy)) = 0.5.4
Provides:       bundled(crate(exr)) = 1.74.0
Provides:       bundled(crate(fastrand)) = 2.3.0
Provides:       bundled(crate(fast-srgb8)) = 1.0.0
Provides:       bundled(crate(fdeflate)) = 0.3.7
Provides:       bundled(crate(ff)) = 0.14.0-pre.0
Provides:       bundled(crate(fiat-crypto)) = 0.3.0
Provides:       bundled(crate(find-msvc-tools)) = 0.1.8
Provides:       bundled(crate(flagset)) = 0.4.7
Provides:       bundled(crate(flate2)) = 1.1.8
Provides:       bundled(crate(float-cmp)) = 0.9.0
Provides:       bundled(crate(float_next_after)) = 1.0.0
Provides:       bundled(crate(fnv)) = 1.0.7
Provides:       bundled(crate(fontconfig-parser)) = 0.5.8
Provides:       bundled(crate(fontdb)) = 0.16.2
Provides:       bundled(crate(fontdb)) = 0.18.0
Provides:       bundled(crate(font-types)) = 0.7.3
Provides:       bundled(crate(foreign-types)) = 0.5.0
Provides:       bundled(crate(foreign-types-macros)) = 0.2.3
Provides:       bundled(crate(foreign-types-shared)) = 0.3.1
Provides:       bundled(crate(form_urlencoded)) = 1.2.2
Provides:       bundled(crate(fragile)) = 2.0.1
Provides:       bundled(crate(fs_extra)) = 1.3.0
Provides:       bundled(crate(funty)) = 2.0.0
Provides:       bundled(crate(fuser)) = 0.15.1
Provides:       bundled(crate(futures)) = 0.3.31
Provides:       bundled(crate(futures-channel)) = 0.3.31
Provides:       bundled(crate(futures-core)) = 0.3.31
Provides:       bundled(crate(futures-executor)) = 0.3.31
Provides:       bundled(crate(futures-io)) = 0.3.31
Provides:       bundled(crate(futures-lite)) = 2.6.1
Provides:       bundled(crate(futures-macro)) = 0.3.31
Provides:       bundled(crate(futures-sink)) = 0.3.31
Provides:       bundled(crate(futures-task)) = 0.3.31
Provides:       bundled(crate(futures-util)) = 0.3.31
Provides:       bundled(crate(generic-array)) = 0.14.7
Provides:       bundled(crate(gethostname)) = 1.1.0
Provides:       bundled(crate(getrandom)) = 0.2.17
Provides:       bundled(crate(getrandom)) = 0.3.4
Provides:       bundled(crate(ghash)) = 0.5.1
Provides:       bundled(crate(ghash)) = 0.6.0-rc.2
Provides:       bundled(crate(gif)) = 0.13.3
Provides:       bundled(crate(gif)) = 0.14.1
Provides:       bundled(crate(glam)) = 0.25.0
Provides:       bundled(crate(gl_generator)) = 0.14.0
Provides:       bundled(crate(glob)) = 0.3.3
Provides:       bundled(crate(glow)) = 0.13.1
Provides:       bundled(crate(glutin_wgl_sys)) = 0.5.0
Provides:       bundled(crate(gpu-alloc)) = 0.6.0
Provides:       bundled(crate(gpu-allocator)) = 0.25.0
Provides:       bundled(crate(gpu-alloc-types)) = 0.3.0
Provides:       bundled(crate(gpu-descriptor)) = 0.2.4
Provides:       bundled(crate(gpu-descriptor-types)) = 0.1.2
Provides:       bundled(crate(group)) = 0.14.0-pre.0
Provides:       bundled(crate(guillotiere)) = 0.6.2
Provides:       bundled(crate(h2)) = 0.4.13
Provides:       bundled(crate(half)) = 2.7.1
Provides:       bundled(crate(hash32)) = 0.2.1
Provides:       bundled(crate(hashbrown)) = 0.12.3
Provides:       bundled(crate(hashbrown)) = 0.14.5
Provides:       bundled(crate(hashbrown)) = 0.16.1
Provides:       bundled(crate(hassle-rs)) = 0.11.0
Provides:       bundled(crate(heapless)) = 0.7.17
Provides:       bundled(crate(heck)) = 0.4.1
Provides:       bundled(crate(heck)) = 0.5.0
Provides:       bundled(crate(hermit-abi)) = 0.5.2
Provides:       bundled(crate(hex)) = 0.4.3
Provides:       bundled(crate(hexf-parse)) = 0.2.1
Provides:       bundled(crate(hickory-proto)) = 0.25.2
Provides:       bundled(crate(hickory-resolver)) = 0.25.2
Provides:       bundled(crate(hkdf)) = 0.12.4
Provides:       bundled(crate(hkdf)) = 0.13.0-rc.2
Provides:       bundled(crate(hmac)) = 0.12.1
Provides:       bundled(crate(hmac)) = 0.13.0-rc.2
Provides:       bundled(crate(hostname)) = 0.4.2
Provides:       bundled(crate(http)) = 1.4.0
Provides:       bundled(crate(httparse)) = 1.10.1
Provides:       bundled(crate(http-body)) = 1.0.1
Provides:       bundled(crate(http-body-util)) = 0.1.3
Provides:       bundled(crate(hybrid-array)) = 0.4.6
Provides:       bundled(crate(hyper)) = 1.8.1
Provides:       bundled(crate(hyper-rustls)) = 0.27.7
Provides:       bundled(crate(hyper-util)) = 0.1.19
Provides:       bundled(crate(iana-time-zone)) = 0.1.65
Provides:       bundled(crate(iana-time-zone-haiku)) = 0.1.2
Provides:       bundled(crate(iced)) = 0.13.1
Provides:       bundled(crate(iced_core)) = 0.13.2
Provides:       bundled(crate(iced_futures)) = 0.13.2
Provides:       bundled(crate(iced_glyphon)) = 0.6.0
Provides:       bundled(crate(iced_graphics)) = 0.13.0
Provides:       bundled(crate(iced_renderer)) = 0.13.0
Provides:       bundled(crate(iced_runtime)) = 0.13.2
Provides:       bundled(crate(iced_tiny_skia)) = 0.13.0
Provides:       bundled(crate(iced_wgpu)) = 0.13.5
Provides:       bundled(crate(iced_widget)) = 0.13.4
Provides:       bundled(crate(iced_winit)) = 0.13.0
Provides:       bundled(crate(icu_collections)) = 2.1.1
Provides:       bundled(crate(icu_locale_core)) = 2.1.1
Provides:       bundled(crate(icu_normalizer)) = 2.1.1
Provides:       bundled(crate(icu_normalizer_data)) = 2.1.1
Provides:       bundled(crate(icu_properties)) = 2.1.2
Provides:       bundled(crate(icu_properties_data)) = 2.1.2
Provides:       bundled(crate(icu_provider)) = 2.1.1
Provides:       bundled(crate(idna)) = 1.1.0
Provides:       bundled(crate(idna_adapter)) = 1.2.1
Provides:       bundled(crate(image)) = 0.24.9
Provides:       bundled(crate(image)) = 0.25.9
Provides:       bundled(crate(imagesize)) = 0.12.0
Provides:       bundled(crate(indexmap)) = 2.13.0
Provides:       bundled(crate(inout)) = 0.1.4
Provides:       bundled(crate(inout)) = 0.2.0-rc.6
Provides:       bundled(crate(instant)) = 0.1.13
Provides:       bundled(crate(ipconfig)) = 0.3.2
Provides:       bundled(crate(ipnet)) = 2.11.0
Provides:       bundled(crate(iri-string)) = 0.7.10
Provides:       bundled(crate(ironrdp)) = 0.14.0
Provides:       bundled(crate(ironrdp-acceptor)) = 0.8.0
Provides:       bundled(crate(ironrdp-ainput)) = 0.5.0
Provides:       bundled(crate(ironrdp-async)) = 0.8.0
Provides:       bundled(crate(ironrdp-cliprdr)) = 0.5.0
Provides:       bundled(crate(ironrdp-connector)) = 0.8.0
Provides:       bundled(crate(ironrdp-core)) = 0.1.5
Provides:       bundled(crate(ironrdp-displaycontrol)) = 0.5.0
Provides:       bundled(crate(ironrdp-dvc)) = 0.5.0
Provides:       bundled(crate(ironrdp-egfx)) = 0.1.0
Provides:       bundled(crate(ironrdp-error)) = 0.1.3
Provides:       bundled(crate(ironrdp-graphics)) = 0.7.0
Provides:       bundled(crate(ironrdp-pdu)) = 0.7.0
Provides:       bundled(crate(ironrdp-rdpsnd)) = 0.7.0
Provides:       bundled(crate(ironrdp-server)) = 0.10.0
Provides:       bundled(crate(ironrdp-svc)) = 0.6.0
Provides:       bundled(crate(ironrdp-tokio)) = 0.8.0
Provides:       bundled(crate(iso7816)) = 0.1.4
Provides:       bundled(crate(iso7816-tlv)) = 0.4.4
Provides:       bundled(crate(is-terminal)) = 0.4.17
Provides:       bundled(crate(is_terminal_polyfill)) = 1.70.2
Provides:       bundled(crate(itertools)) = 0.10.5
Provides:       bundled(crate(itertools)) = 0.12.1
Provides:       bundled(crate(itertools)) = 0.13.0
Provides:       bundled(crate(itoa)) = 1.0.17
Provides:       bundled(crate(jni)) = 0.21.1
Provides:       bundled(crate(jni-sys)) = 0.3.0
Provides:       bundled(crate(jobserver)) = 0.1.34
Provides:       bundled(crate(jpeg-decoder)) = 0.3.2
Provides:       bundled(crate(js-sys)) = 0.3.85
Provides:       bundled(crate(kamadak-exif)) = 0.5.5
Provides:       bundled(crate(keccak)) = 0.2.0-rc.0
Provides:       bundled(crate(khronos_api)) = 3.1.0
Provides:       bundled(crate(khronos-egl)) = 6.0.0
Provides:       bundled(crate(kurbo)) = 0.10.4
Provides:       bundled(crate(kurbo)) = 0.11.3
Provides:       bundled(crate(lamco-clipboard-core)) = 0.5.0
Provides:       bundled(crate(lamco-pipewire)) = 0.1.4
Provides:       bundled(crate(lamco-portal)) = 0.3.1
Provides:       bundled(crate(lamco-rdp)) = 0.5.0
Provides:       bundled(crate(lamco-rdp-input)) = 0.1.1
Provides:       bundled(crate(lamco-video)) = 0.1.2
Provides:       bundled(crate(lamco-wayland)) = 0.2.5
Provides:       bundled(crate(lazycell)) = 1.3.0
Provides:       bundled(crate(lazy_static)) = 1.5.0
Provides:       bundled(crate(lebe)) = 0.5.3
Provides:       bundled(crate(libc)) = 0.2.180
Provides:       bundled(crate(libloading)) = 0.7.4
Provides:       bundled(crate(libloading)) = 0.8.9
Provides:       bundled(crate(libm)) = 0.2.16
Provides:       bundled(crate(libopus_sys)) = 0.3.3
Provides:       bundled(crate(libredox)) = 0.1.12
Provides:       bundled(crate(libspa)) = 0.8.0
Provides:       bundled(crate(libspa-sys)) = 0.8.0
Provides:       bundled(crate(libz-sys)) = 1.1.23
Provides:       bundled(crate(linux-raw-sys)) = 0.11.0
Provides:       bundled(crate(linux-raw-sys)) = 0.4.15
Provides:       bundled(crate(linux-raw-sys)) = 0.6.5
Provides:       bundled(crate(litemap)) = 0.8.1
Provides:       bundled(crate(lock_api)) = 0.4.14
Provides:       bundled(crate(log)) = 0.4.29
Provides:       bundled(crate(lru)) = 0.12.5
Provides:       bundled(crate(lyon)) = 1.0.16
Provides:       bundled(crate(lyon_algorithms)) = 1.0.16
Provides:       bundled(crate(lyon_geom)) = 1.0.18
Provides:       bundled(crate(lyon_path)) = 1.0.16
Provides:       bundled(crate(lyon_tessellation)) = 1.0.16
Provides:       bundled(crate(malloc_buf)) = 0.0.6
Provides:       bundled(crate(matchers)) = 0.2.0
Provides:       bundled(crate(md4)) = 0.10.2
Provides:       bundled(crate(md-5)) = 0.10.6
Provides:       bundled(crate(md-5)) = 0.11.0-rc.2
Provides:       bundled(crate(memchr)) = 2.7.6
Provides:       bundled(crate(memmap2)) = 0.8.0
Provides:       bundled(crate(memmap2)) = 0.9.9
Provides:       bundled(crate(memoffset)) = 0.9.1
Provides:       bundled(crate(metal)) = 0.27.0
Provides:       bundled(crate(minimal-lexical)) = 0.2.1
Provides:       bundled(crate(miniz_oxide)) = 0.8.9
Provides:       bundled(crate(mio)) = 1.1.1
Provides:       bundled(crate(mockall)) = 0.12.1
Provides:       bundled(crate(mockall_derive)) = 0.12.1
Provides:       bundled(crate(moka)) = 0.12.13
Provides:       bundled(crate(moxcms)) = 0.7.11
Provides:       bundled(crate(mutate_once)) = 0.1.2
Provides:       bundled(crate(naga)) = 0.19.2
Provides:       bundled(crate(nasm-rs)) = 0.3.1
Provides:       bundled(crate(ndk)) = 0.9.0
Provides:       bundled(crate(ndk-context)) = 0.1.1
Provides:       bundled(crate(ndk-sys)) = 0.5.0
Provides:       bundled(crate(ndk-sys)) = 0.6.0
Provides:       bundled(crate(nix)) = 0.27.1
Provides:       bundled(crate(nix)) = 0.29.0
Provides:       bundled(crate(nom)) = 7.1.3
Provides:       bundled(crate(ntapi)) = 0.4.2
Provides:       bundled(crate(nu-ansi-term)) = 0.50.3
Provides:       bundled(crate(num)) = 0.4.3
Provides:       bundled(crate(num-bigint)) = 0.4.6
Provides:       bundled(crate(num-complex)) = 0.4.6
Provides:       bundled(crate(num-conv)) = 0.1.0
Provides:       bundled(crate(num_cpus)) = 1.17.0
Provides:       bundled(crate(num-derive)) = 0.4.2
Provides:       bundled(crate(num_enum)) = 0.7.5
Provides:       bundled(crate(num_enum_derive)) = 0.7.5
Provides:       bundled(crate(num-integer)) = 0.1.46
Provides:       bundled(crate(num-iter)) = 0.1.45
Provides:       bundled(crate(num-rational)) = 0.4.2
Provides:       bundled(crate(num-traits)) = 0.2.19
Provides:       bundled(crate(nvidia-video-codec-sdk)) = 0.4.0
Provides:       bundled(crate(objc)) = 0.2.7
Provides:       bundled(crate(objc2)) = 0.5.2
Provides:       bundled(crate(objc2)) = 0.6.3
Provides:       bundled(crate(objc2-app-kit)) = 0.2.2
Provides:       bundled(crate(objc2-app-kit)) = 0.3.2
Provides:       bundled(crate(objc2-cloud-kit)) = 0.2.2
Provides:       bundled(crate(objc2-contacts)) = 0.2.2
Provides:       bundled(crate(objc2-core-data)) = 0.2.2
Provides:       bundled(crate(objc2-core-foundation)) = 0.3.2
Provides:       bundled(crate(objc2-core-graphics)) = 0.3.2
Provides:       bundled(crate(objc2-core-image)) = 0.2.2
Provides:       bundled(crate(objc2-core-location)) = 0.2.2
Provides:       bundled(crate(objc2-encode)) = 4.1.0
Provides:       bundled(crate(objc2-foundation)) = 0.2.2
Provides:       bundled(crate(objc2-foundation)) = 0.3.2
Provides:       bundled(crate(objc2-io-surface)) = 0.3.2
Provides:       bundled(crate(objc2-link-presentation)) = 0.2.2
Provides:       bundled(crate(objc2-metal)) = 0.2.2
Provides:       bundled(crate(objc2-quartz-core)) = 0.2.2
Provides:       bundled(crate(objc2-quartz-core)) = 0.3.2
Provides:       bundled(crate(objc2-symbols)) = 0.2.2
Provides:       bundled(crate(objc2-ui-kit)) = 0.2.2
Provides:       bundled(crate(objc2-uniform-type-identifiers)) = 0.2.2
Provides:       bundled(crate(objc2-user-notifications)) = 0.2.2
Provides:       bundled(crate(objc_exception)) = 0.1.2
Provides:       bundled(crate(objc-sys)) = 0.3.5
Provides:       bundled(crate(oid)) = 0.2.1
Provides:       bundled(crate(once_cell)) = 1.21.3
Provides:       bundled(crate(once_cell_polyfill)) = 1.70.2
Provides:       bundled(crate(oorandom)) = 11.1.5
Provides:       bundled(crate(opaque-debug)) = 0.3.1
Provides:       bundled(crate(openh264)) = 0.9.3
Provides:       bundled(crate(openh264-sys2)) = 0.9.3
Provides:       bundled(crate(openssl-probe)) = 0.2.1
Provides:       bundled(crate(option-ext)) = 0.2.0
Provides:       bundled(crate(opus2)) = 0.3.3
Provides:       bundled(crate(orbclient)) = 0.3.50
Provides:       bundled(crate(ordered-multimap)) = 0.4.3
Provides:       bundled(crate(ordered-stream)) = 0.2.0
Provides:       bundled(crate(ouroboros)) = 0.18.5
Provides:       bundled(crate(ouroboros_macro)) = 0.18.5
Provides:       bundled(crate(owned_ttf_parser)) = 0.25.1
Provides:       bundled(crate(p256)) = 0.14.0-pre.11
Provides:       bundled(crate(p384)) = 0.14.0-pre.11
Provides:       bundled(crate(p521)) = 0.14.0-pre.11
Provides:       bundled(crate(page_size)) = 0.6.0
Provides:       bundled(crate(palette)) = 0.7.6
Provides:       bundled(crate(palette_derive)) = 0.7.6
Provides:       bundled(crate(pam)) = 0.7.0
Provides:       bundled(crate(pam-sys)) = 0.5.6
Provides:       bundled(crate(parking)) = 2.2.1
Provides:       bundled(crate(parking_lot)) = 0.11.2
Provides:       bundled(crate(parking_lot)) = 0.12.5
Provides:       bundled(crate(parking_lot_core)) = 0.8.6
Provides:       bundled(crate(parking_lot_core)) = 0.9.12
Provides:       bundled(crate(paste)) = 1.0.15
Provides:       bundled(crate(pbkdf2)) = 0.13.0-rc.1
Provides:       bundled(crate(pem)) = 3.0.6
Provides:       bundled(crate(pem-rfc7468)) = 0.7.0
Provides:       bundled(crate(pem-rfc7468)) = 1.0.0-rc.3
Provides:       bundled(crate(percent-encoding)) = 2.3.2
Provides:       bundled(crate(phf)) = 0.11.3
Provides:       bundled(crate(phf_generator)) = 0.11.3
Provides:       bundled(crate(phf_macros)) = 0.11.3
Provides:       bundled(crate(phf_shared)) = 0.11.3
Provides:       bundled(crate(picky)) = 7.0.0-rc.20
Provides:       bundled(crate(picky-asn1)) = 0.10.1
Provides:       bundled(crate(picky-asn1-der)) = 0.5.4
Provides:       bundled(crate(picky-asn1-x509)) = 0.15.2
Provides:       bundled(crate(picky-krb)) = 0.12.0
Provides:       bundled(crate(pico-args)) = 0.5.0
Provides:       bundled(crate(pin-project)) = 1.1.10
Provides:       bundled(crate(pin-project-internal)) = 1.1.10
Provides:       bundled(crate(pin-project-lite)) = 0.2.16
Provides:       bundled(crate(pin-utils)) = 0.1.0
Provides:       bundled(crate(piper)) = 0.2.4
Provides:       bundled(crate(pipewire)) = 0.8.0
Provides:       bundled(crate(pipewire-sys)) = 0.8.0
Provides:       bundled(crate(pkcs1)) = 0.7.5
Provides:       bundled(crate(pkcs1)) = 0.8.0-rc.4
Provides:       bundled(crate(pkcs8)) = 0.11.0-rc.7
Provides:       bundled(crate(pkg-config)) = 0.3.32
Provides:       bundled(crate(plotters)) = 0.3.7
Provides:       bundled(crate(plotters-backend)) = 0.3.7
Provides:       bundled(crate(plotters-svg)) = 0.3.7
Provides:       bundled(crate(png)) = 0.17.16
Provides:       bundled(crate(png)) = 0.18.0
Provides:       bundled(crate(polling)) = 3.11.0
Provides:       bundled(crate(pollster)) = 0.4.0
Provides:       bundled(crate(polyval)) = 0.6.2
Provides:       bundled(crate(polyval)) = 0.7.0-rc.2
Provides:       bundled(crate(portable-atomic)) = 1.13.0
Provides:       bundled(crate(portpicker)) = 0.1.1
Provides:       bundled(crate(potential_utf)) = 0.1.4
Provides:       bundled(crate(powerfmt)) = 0.2.0
Provides:       bundled(crate(ppv-lite86)) = 0.2.21
Provides:       bundled(crate(predicates)) = 3.1.3
Provides:       bundled(crate(predicates-core)) = 1.0.9
Provides:       bundled(crate(predicates-tree)) = 1.0.12
Provides:       bundled(crate(presser)) = 0.3.1
Provides:       bundled(crate(prettyplease)) = 0.2.37
Provides:       bundled(crate(primefield)) = 0.14.0-pre.6
Provides:       bundled(crate(primeorder)) = 0.14.0-pre.9
Provides:       bundled(crate(proc-macro2)) = 1.0.106
Provides:       bundled(crate(proc-macro2-diagnostics)) = 0.10.1
Provides:       bundled(crate(proc-macro-crate)) = 3.4.0
Provides:       bundled(crate(profiling)) = 1.0.17
Provides:       bundled(crate(proptest)) = 1.9.0
Provides:       bundled(crate(pxfm)) = 0.1.27
Provides:       bundled(crate(qoi)) = 0.4.1
Provides:       bundled(crate(qoicoubeh)) = 0.5.0
Provides:       bundled(crate(quick-error)) = 1.2.3
Provides:       bundled(crate(quick-xml)) = 0.38.4
Provides:       bundled(crate(quote)) = 1.0.44
Provides:       bundled(crate(radium)) = 0.7.0
Provides:       bundled(crate(rand)) = 0.8.5
Provides:       bundled(crate(rand)) = 0.9.2
Provides:       bundled(crate(rand_chacha)) = 0.3.1
Provides:       bundled(crate(rand_chacha)) = 0.9.0
Provides:       bundled(crate(rand_core)) = 0.6.4
Provides:       bundled(crate(rand_core)) = 0.9.5
Provides:       bundled(crate(rand_xorshift)) = 0.4.0
Provides:       bundled(crate(range-alloc)) = 0.1.4
Provides:       bundled(crate(rangemap)) = 1.7.1
Provides:       bundled(crate(raw-window-handle)) = 0.6.2
Provides:       bundled(crate(rayon)) = 1.11.0
Provides:       bundled(crate(rayon-core)) = 1.13.0
Provides:       bundled(crate(rc2)) = 0.9.0-pre.0
Provides:       bundled(crate(rcgen)) = 0.12.1
Provides:       bundled(crate(read-fonts)) = 0.22.7
Provides:       bundled(crate(redox_syscall)) = 0.2.16
Provides:       bundled(crate(redox_syscall)) = 0.4.1
Provides:       bundled(crate(redox_syscall)) = 0.5.18
Provides:       bundled(crate(redox_syscall)) = 0.7.0
Provides:       bundled(crate(redox_users)) = 0.4.6
Provides:       bundled(crate(r-efi)) = 5.3.0
Provides:       bundled(crate(regex)) = 1.12.2
Provides:       bundled(crate(regex-automata)) = 0.4.13
Provides:       bundled(crate(regex-syntax)) = 0.8.8
Provides:       bundled(crate(reis)) = 0.5.0
Provides:       bundled(crate(renderdoc-sys)) = 1.1.0
Provides:       bundled(crate(reqwest)) = 0.12.28
Provides:       bundled(crate(resolv-conf)) = 0.7.6
Provides:       bundled(crate(resvg)) = 0.42.0
Provides:       bundled(crate(rfc6979)) = 0.5.0-rc.1
Provides:       bundled(crate(rfd)) = 0.15.4
Provides:       bundled(crate(rgb)) = 0.8.52
Provides:       bundled(crate(ring)) = 0.17.14
Provides:       bundled(crate(roxmltree)) = 0.20.0
Provides:       bundled(crate(rsa)) = 0.10.0-rc.9
Provides:       bundled(crate(rustc-hash)) = 1.1.0
Provides:       bundled(crate(rustc-hash)) = 2.1.1
Provides:       bundled(crate(rustc_version)) = 0.4.1
Provides:       bundled(crate(rusticata-macros)) = 4.1.0
Provides:       bundled(crate(rust-ini)) = 0.18.0
Provides:       bundled(crate(rustix)) = 0.38.44
Provides:       bundled(crate(rustix)) = 1.1.3
Provides:       bundled(crate(rustls)) = 0.23.36
Provides:       bundled(crate(rustls-native-certs)) = 0.8.3
Provides:       bundled(crate(rustls-pemfile)) = 2.2.0
Provides:       bundled(crate(rustls-pki-types)) = 1.14.0
Provides:       bundled(crate(rustls-webpki)) = 0.103.9
Provides:       bundled(crate(rustversion)) = 1.0.22
Provides:       bundled(crate(rustybuzz)) = 0.14.1
Provides:       bundled(crate(rusty-fork)) = 0.3.1
Provides:       bundled(crate(ryu)) = 1.0.22
Provides:       bundled(crate(safe_arch)) = 0.7.4
Provides:       bundled(crate(same-file)) = 1.0.6
Provides:       bundled(crate(schannel)) = 0.1.28
Provides:       bundled(crate(scoped-tls)) = 1.0.1
Provides:       bundled(crate(scopeguard)) = 1.2.0
Provides:       bundled(crate(sctk-adwaita)) = 0.10.1
Provides:       bundled(crate(sec1)) = 0.8.0-rc.10
Provides:       bundled(crate(secrecy)) = 0.8.0
Provides:       bundled(crate(secret-service)) = 5.1.0
Provides:       bundled(crate(security-framework)) = 3.5.1
Provides:       bundled(crate(security-framework-sys)) = 2.15.0
Provides:       bundled(crate(self_cell)) = 1.2.2
Provides:       bundled(crate(semver)) = 1.0.27
Provides:       bundled(crate(serde)) = 1.0.228
Provides:       bundled(crate(serde_bytes)) = 0.11.19
Provides:       bundled(crate(serde_core)) = 1.0.228
Provides:       bundled(crate(serdect)) = 0.4.2
Provides:       bundled(crate(serde_derive)) = 1.0.228
Provides:       bundled(crate(serde_json)) = 1.0.149
Provides:       bundled(crate(serde_repr)) = 0.1.20
Provides:       bundled(crate(serde_spanned)) = 0.6.9
Provides:       bundled(crate(serde_urlencoded)) = 0.7.1
Provides:       bundled(crate(sha1)) = 0.10.6
Provides:       bundled(crate(sha1)) = 0.11.0-rc.2
Provides:       bundled(crate(sha2)) = 0.10.9
Provides:       bundled(crate(sha2)) = 0.11.0-rc.2
Provides:       bundled(crate(sha3)) = 0.11.0-rc.3
Provides:       bundled(crate(sharded-slab)) = 0.1.7
Provides:       bundled(crate(shlex)) = 1.3.0
Provides:       bundled(crate(signal-hook-registry)) = 1.4.8
Provides:       bundled(crate(signature)) = 3.0.0-rc.4
Provides:       bundled(crate(simd-adler32)) = 0.3.8
Provides:       bundled(crate(simplecss)) = 0.2.2
Provides:       bundled(crate(siphasher)) = 1.0.2
Provides:       bundled(crate(skrifa)) = 0.22.3
Provides:       bundled(crate(slab)) = 0.4.11
Provides:       bundled(crate(slotmap)) = 1.1.1
Provides:       bundled(crate(smallvec)) = 1.15.1
Provides:       bundled(crate(smithay-client-toolkit)) = 0.19.2
Provides:       bundled(crate(smithay-client-toolkit)) = 0.20.0
Provides:       bundled(crate(smithay-clipboard)) = 0.7.3
Provides:       bundled(crate(smol_str)) = 0.2.2
Provides:       bundled(crate(socket2)) = 0.5.10
Provides:       bundled(crate(socket2)) = 0.6.2
Provides:       bundled(crate(softbuffer)) = 0.4.8
Provides:       bundled(crate(spin)) = 0.9.8
Provides:       bundled(crate(spirv)) = 0.3.0
Provides:       bundled(crate(spki)) = 0.7.3
Provides:       bundled(crate(spki)) = 0.8.0-rc.4
Provides:       bundled(crate(sspi)) = 0.18.7
Provides:       bundled(crate(stable_deref_trait)) = 1.2.1
Provides:       bundled(crate(static_assertions)) = 1.1.0
Provides:       bundled(crate(strict-num)) = 0.1.1
Provides:       bundled(crate(strsim)) = 0.11.1
Provides:       bundled(crate(subtle)) = 2.6.1
Provides:       bundled(crate(svg_fmt)) = 0.4.5
Provides:       bundled(crate(svgtypes)) = 0.15.3
Provides:       bundled(crate(swash)) = 0.1.19
Provides:       bundled(crate(syn)) = 1.0.109
Provides:       bundled(crate(syn)) = 2.0.114
Provides:       bundled(crate(sync_wrapper)) = 1.0.2
Provides:       bundled(crate(synstructure)) = 0.13.2
Provides:       bundled(crate(sysinfo)) = 0.30.13
Provides:       bundled(crate(sys-locale)) = 0.3.2
Provides:       bundled(crate(system-configuration)) = 0.6.1
Provides:       bundled(crate(system-configuration-sys)) = 0.6.0
Provides:       bundled(crate(system-deps)) = 6.2.2
Provides:       bundled(crate(tagptr)) = 0.2.0
Provides:       bundled(crate(tap)) = 1.0.1
Provides:       bundled(crate(target-lexicon)) = 0.12.16
Provides:       bundled(crate(tempfile)) = 3.24.0
Provides:       bundled(crate(termcolor)) = 1.4.1
Provides:       bundled(crate(termtree)) = 0.5.1
Provides:       bundled(crate(thiserror)) = 1.0.69
Provides:       bundled(crate(thiserror)) = 2.0.18
Provides:       bundled(crate(thiserror-impl)) = 1.0.69
Provides:       bundled(crate(thiserror-impl)) = 2.0.18
Provides:       bundled(crate(thread_local)) = 1.1.9
Provides:       bundled(crate(tiff)) = 0.9.1
Provides:       bundled(crate(time)) = 0.3.45
Provides:       bundled(crate(time-core)) = 0.1.7
Provides:       bundled(crate(time-macros)) = 0.2.25
Provides:       bundled(crate(tiny-skia)) = 0.11.4
Provides:       bundled(crate(tiny-skia-path)) = 0.11.4
Provides:       bundled(crate(tinystr)) = 0.8.2
Provides:       bundled(crate(tinytemplate)) = 1.2.1
Provides:       bundled(crate(tinyvec)) = 1.10.0
Provides:       bundled(crate(tinyvec_macros)) = 0.1.1
Provides:       bundled(crate(tiny-xlib)) = 0.2.4
Provides:       bundled(crate(tls_codec)) = 0.4.2
Provides:       bundled(crate(tls_codec_derive)) = 0.4.2
Provides:       bundled(crate(tokio)) = 1.49.0
Provides:       bundled(crate(tokio-macros)) = 2.6.0
Provides:       bundled(crate(tokio-rustls)) = 0.26.4
Provides:       bundled(crate(tokio-util)) = 0.7.18
Provides:       bundled(crate(toml)) = 0.8.23
Provides:       bundled(crate(toml_datetime)) = 0.6.11
Provides:       bundled(crate(toml_datetime)) = 0.7.5
Provides:       bundled(crate(toml_edit)) = 0.22.27
Provides:       bundled(crate(toml_edit)) = 0.23.10
Provides:       bundled(crate(toml_parser)) = 1.0.6
Provides:       bundled(crate(toml_write)) = 0.1.2
Provides:       bundled(crate(tower)) = 0.5.3
Provides:       bundled(crate(tower-http)) = 0.6.8
Provides:       bundled(crate(tower-layer)) = 0.3.3
Provides:       bundled(crate(tower-service)) = 0.3.3
Provides:       bundled(crate(tracing)) = 0.1.44
Provides:       bundled(crate(tracing-appender)) = 0.2.4
Provides:       bundled(crate(tracing-attributes)) = 0.1.31
Provides:       bundled(crate(tracing-core)) = 0.1.36
Provides:       bundled(crate(tracing-log)) = 0.2.0
Provides:       bundled(crate(tracing-serde)) = 0.2.0
Provides:       bundled(crate(tracing-subscriber)) = 0.3.22
Provides:       bundled(crate(try-lock)) = 0.2.5
Provides:       bundled(crate(ttf-parser)) = 0.20.0
Provides:       bundled(crate(ttf-parser)) = 0.21.1
Provides:       bundled(crate(ttf-parser)) = 0.25.1
Provides:       bundled(crate(typenum)) = 1.19.0
Provides:       bundled(crate(uds_windows)) = 1.1.0
Provides:       bundled(crate(unarray)) = 0.1.4
Provides:       bundled(crate(unicode-bidi)) = 0.3.18
Provides:       bundled(crate(unicode-bidi-mirroring)) = 0.2.0
Provides:       bundled(crate(unicode-ccc)) = 0.2.0
Provides:       bundled(crate(unicode-ident)) = 1.0.22
Provides:       bundled(crate(unicode-linebreak)) = 0.1.5
Provides:       bundled(crate(unicode-properties)) = 0.1.4
Provides:       bundled(crate(unicode-script)) = 0.5.8
Provides:       bundled(crate(unicode-segmentation)) = 1.12.0
Provides:       bundled(crate(unicode-vo)) = 0.1.0
Provides:       bundled(crate(unicode-width)) = 0.1.14
Provides:       bundled(crate(unicode-xid)) = 0.2.6
Provides:       bundled(crate(universal-hash)) = 0.5.1
Provides:       bundled(crate(universal-hash)) = 0.6.0-rc.2
Provides:       bundled(crate(untrusted)) = 0.9.0
Provides:       bundled(crate(url)) = 2.5.8
Provides:       bundled(crate(urlencoding)) = 2.1.3
Provides:       bundled(crate(users)) = 0.8.1
Provides:       bundled(crate(usvg)) = 0.42.0
Provides:       bundled(crate(utf8_iter)) = 1.0.4
Provides:       bundled(crate(utf8parse)) = 0.2.2
Provides:       bundled(crate(uuid)) = 1.20.0
Provides:       bundled(crate(valuable)) = 0.1.1
Provides:       bundled(crate(vcpkg)) = 0.2.15
Provides:       bundled(crate(version_check)) = 0.9.5
Provides:       bundled(crate(version-compare)) = 0.2.1
Provides:       bundled(crate(wait-timeout)) = 0.2.1
Provides:       bundled(crate(walkdir)) = 2.5.0
Provides:       bundled(crate(want)) = 0.3.1
Provides:       bundled(crate(wasi)) = 0.11.1
Provides:       bundled(crate(wasip2)) = 1.0.2
Provides:       bundled(crate(wasm-bindgen)) = 0.2.108
Provides:       bundled(crate(wasm-bindgen-futures)) = 0.4.58
Provides:       bundled(crate(wasm-bindgen-macro)) = 0.2.108
Provides:       bundled(crate(wasm-bindgen-macro-support)) = 0.2.108
Provides:       bundled(crate(wasm-bindgen-shared)) = 0.2.108
Provides:       bundled(crate(wasm-timer)) = 0.2.5
Provides:       bundled(crate(wayland-backend)) = 0.3.12
Provides:       bundled(crate(wayland-client)) = 0.31.12
Provides:       bundled(crate(wayland-csd-frame)) = 0.3.0
Provides:       bundled(crate(wayland-cursor)) = 0.31.12
Provides:       bundled(crate(wayland-protocols)) = 0.31.2
Provides:       bundled(crate(wayland-protocols)) = 0.32.10
Provides:       bundled(crate(wayland-protocols-experimental)) = 20250721.0.1
Provides:       bundled(crate(wayland-protocols-misc)) = 0.2.0
Provides:       bundled(crate(wayland-protocols-misc)) = 0.3.10
Provides:       bundled(crate(wayland-protocols-plasma)) = 0.3.10
Provides:       bundled(crate(wayland-protocols-wlr)) = 0.3.10
Provides:       bundled(crate(wayland-scanner)) = 0.31.8
Provides:       bundled(crate(wayland-sys)) = 0.31.8
Provides:       bundled(crate(web-sys)) = 0.3.85
Provides:       bundled(crate(web-time)) = 1.1.0
Provides:       bundled(crate(weezl)) = 0.1.12
Provides:       bundled(crate(wgpu)) = 0.19.4
Provides:       bundled(crate(wgpu-core)) = 0.19.4
Provides:       bundled(crate(wgpu-hal)) = 0.19.5
Provides:       bundled(crate(wgpu-types)) = 0.19.2
Provides:       bundled(crate(wide)) = 0.7.33
Provides:       bundled(crate(widestring)) = 1.2.1
Provides:       bundled(crate(winapi)) = 0.3.9
Provides:       bundled(crate(winapi-i686-pc-windows-gnu)) = 0.4.0
Provides:       bundled(crate(winapi-util)) = 0.1.11
Provides:       bundled(crate(winapi-x86_64-pc-windows-gnu)) = 0.4.0
Provides:       bundled(crate(window_clipboard)) = 0.4.1
Provides:       bundled(crate(windows)) = 0.52.0
Provides:       bundled(crate(windows)) = 0.62.2
Provides:       bundled(crate(windows_aarch64_gnullvm)) = 0.42.2
Provides:       bundled(crate(windows_aarch64_gnullvm)) = 0.48.5
Provides:       bundled(crate(windows_aarch64_gnullvm)) = 0.52.6
Provides:       bundled(crate(windows_aarch64_gnullvm)) = 0.53.1
Provides:       bundled(crate(windows_aarch64_msvc)) = 0.42.2
Provides:       bundled(crate(windows_aarch64_msvc)) = 0.48.5
Provides:       bundled(crate(windows_aarch64_msvc)) = 0.52.6
Provides:       bundled(crate(windows_aarch64_msvc)) = 0.53.1
Provides:       bundled(crate(windows-collections)) = 0.3.2
Provides:       bundled(crate(windows-core)) = 0.52.0
Provides:       bundled(crate(windows-core)) = 0.62.2
Provides:       bundled(crate(windows-future)) = 0.3.2
Provides:       bundled(crate(windows_i686_gnu)) = 0.42.2
Provides:       bundled(crate(windows_i686_gnu)) = 0.48.5
Provides:       bundled(crate(windows_i686_gnu)) = 0.52.6
Provides:       bundled(crate(windows_i686_gnu)) = 0.53.1
Provides:       bundled(crate(windows_i686_gnullvm)) = 0.52.6
Provides:       bundled(crate(windows_i686_gnullvm)) = 0.53.1
Provides:       bundled(crate(windows_i686_msvc)) = 0.42.2
Provides:       bundled(crate(windows_i686_msvc)) = 0.48.5
Provides:       bundled(crate(windows_i686_msvc)) = 0.52.6
Provides:       bundled(crate(windows_i686_msvc)) = 0.53.1
Provides:       bundled(crate(windows-implement)) = 0.60.2
Provides:       bundled(crate(windows-interface)) = 0.59.3
Provides:       bundled(crate(windows-link)) = 0.2.1
Provides:       bundled(crate(windows-numerics)) = 0.3.1
Provides:       bundled(crate(windows-registry)) = 0.6.1
Provides:       bundled(crate(windows-result)) = 0.4.1
Provides:       bundled(crate(windows-strings)) = 0.5.1
Provides:       bundled(crate(windows-sys)) = 0.45.0
Provides:       bundled(crate(windows-sys)) = 0.48.0
Provides:       bundled(crate(windows-sys)) = 0.52.0
Provides:       bundled(crate(windows-sys)) = 0.59.0
Provides:       bundled(crate(windows-sys)) = 0.60.2
Provides:       bundled(crate(windows-sys)) = 0.61.2
Provides:       bundled(crate(windows-targets)) = 0.42.2
Provides:       bundled(crate(windows-targets)) = 0.48.5
Provides:       bundled(crate(windows-targets)) = 0.52.6
Provides:       bundled(crate(windows-targets)) = 0.53.5
Provides:       bundled(crate(windows-threading)) = 0.2.1
Provides:       bundled(crate(windows_x86_64_gnu)) = 0.42.2
Provides:       bundled(crate(windows_x86_64_gnu)) = 0.48.5
Provides:       bundled(crate(windows_x86_64_gnu)) = 0.52.6
Provides:       bundled(crate(windows_x86_64_gnu)) = 0.53.1
Provides:       bundled(crate(windows_x86_64_gnullvm)) = 0.42.2
Provides:       bundled(crate(windows_x86_64_gnullvm)) = 0.48.5
Provides:       bundled(crate(windows_x86_64_gnullvm)) = 0.52.6
Provides:       bundled(crate(windows_x86_64_gnullvm)) = 0.53.1
Provides:       bundled(crate(windows_x86_64_msvc)) = 0.42.2
Provides:       bundled(crate(windows_x86_64_msvc)) = 0.48.5
Provides:       bundled(crate(windows_x86_64_msvc)) = 0.52.6
Provides:       bundled(crate(windows_x86_64_msvc)) = 0.53.1
Provides:       bundled(crate(winit)) = 0.30.12
Provides:       bundled(crate(winnow)) = 0.7.14
Provides:       bundled(crate(winreg)) = 0.10.1
Provides:       bundled(crate(winreg)) = 0.50.0
Provides:       bundled(crate(winscard)) = 0.2.5
Provides:       bundled(crate(wit-bindgen)) = 0.51.0
Provides:       bundled(crate(writeable)) = 0.6.2
Provides:       bundled(crate(wyz)) = 0.5.1
Provides:       bundled(crate(x11-dl)) = 2.21.0
Provides:       bundled(crate(x11rb)) = 0.13.2
Provides:       bundled(crate(x11rb-protocol)) = 0.13.2
Provides:       bundled(crate(x25519-dalek)) = 3.0.0-pre.1
Provides:       bundled(crate(x509-cert)) = 0.2.5
Provides:       bundled(crate(xcursor)) = 0.3.10
Provides:       bundled(crate(xdg-home)) = 1.3.0
Provides:       bundled(crate(xkbcommon)) = 0.7.0
Provides:       bundled(crate(xkbcommon-dl)) = 0.4.2
Provides:       bundled(crate(xkeysym)) = 0.2.1
Provides:       bundled(crate(xml-rs)) = 0.8.28
Provides:       bundled(crate(xmlwriter)) = 0.1.0
Provides:       bundled(crate(yansi)) = 1.0.1
Provides:       bundled(crate(yansi-term)) = 0.1.2
Provides:       bundled(crate(yasna)) = 0.5.2
Provides:       bundled(crate(yazi)) = 0.1.6
Provides:       bundled(crate(yoke)) = 0.8.1
Provides:       bundled(crate(yoke-derive)) = 0.8.1
Provides:       bundled(crate(yuv)) = 0.8.10
Provides:       bundled(crate(zbus)) = 4.4.0
Provides:       bundled(crate(zbus)) = 5.13.2
Provides:       bundled(crate(zbus_macros)) = 4.4.0
Provides:       bundled(crate(zbus_macros)) = 5.13.2
Provides:       bundled(crate(zbus_names)) = 3.0.0
Provides:       bundled(crate(zbus_names)) = 4.3.1
Provides:       bundled(crate(zeno)) = 0.2.3
Provides:       bundled(crate(zerocopy)) = 0.8.36
Provides:       bundled(crate(zerocopy-derive)) = 0.8.36
Provides:       bundled(crate(zerofrom)) = 0.1.6
Provides:       bundled(crate(zerofrom-derive)) = 0.1.6
Provides:       bundled(crate(zeroize)) = 1.8.2
Provides:       bundled(crate(zeroize_derive)) = 1.4.3
Provides:       bundled(crate(zerotrie)) = 0.2.3
Provides:       bundled(crate(zerovec)) = 0.11.5
Provides:       bundled(crate(zerovec-derive)) = 0.11.2
Provides:       bundled(crate(zmij)) = 1.0.17
Provides:       bundled(crate(zstd-safe)) = 7.2.4
Provides:       bundled(crate(zstd-sys)) = 2.0.16
Provides:       bundled(crate(zune-core)) = 0.5.1
Provides:       bundled(crate(zune-inflate)) = 0.2.54
Provides:       bundled(crate(zune-jpeg)) = 0.5.12
Provides:       bundled(crate(zvariant)) = 4.2.0
Provides:       bundled(crate(zvariant)) = 5.9.2
Provides:       bundled(crate(zvariant_derive)) = 4.2.0
Provides:       bundled(crate(zvariant_derive)) = 5.9.2
Provides:       bundled(crate(zvariant_utils)) = 2.1.0
Provides:       bundled(crate(zvariant_utils)) = 3.3.0
%description
lamco-rdp-server is a high-performance RDP server for Wayland-based Linux
desktops. It provides automatic capability detection to select the appropriate
screen capture and input methods for the running environment.

Features:
- H.264 video encoding via EGFX channel (AVC420/AVC444)
- Hardware-accelerated encoding (VA-API, NVENC)
- Multi-monitor support
- Clipboard synchronization
- Keyboard and mouse input
- Automatic platform and DE detection
- Full-featured configuration GUI (10-tab interface)

%prep
%setup -q
%patch -P 0 -p1
# Clear vendored cros-libva checksum so cargo doesn't reject the patched file
sed -i 's/"files":{[^}]*}/"files":{}/' vendor/cros-libva/.cargo-checksum.json

%build
# Use vendored dependencies (tarball includes vendor/ and .cargo/config.toml)
export CARGO_HOME="$PWD/.cargo"
export CARGO_TARGET_DIR="$PWD/target"

# Override release profile for distro build constraints:
# - Thin LTO instead of fat: reduces peak memory from ~10GB to ~4GB
# - codegen-units=4: allows parallel codegen, reduces build time
# Upstream Cargo.toml uses lto=true/codegen-units=1 for maximum optimization;
# these overrides adapt for mock/koji resource limits while preserving >96%
# of runtime performance. No features are affected.
export CARGO_PROFILE_RELEASE_LTO=thin
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=4

# Build release binaries (server + GUI)
cargo build --release --offline --features "default,vaapi,gui"

%install
install -Dm755 target/release/%{name} %{buildroot}%{_bindir}/%{name}
install -Dm755 target/release/%{name}-gui %{buildroot}%{_bindir}/%{name}-gui

# Config directory (server creates default config on first run)
install -dm755 %{buildroot}%{_sysconfdir}/%{name}

# Systemd user service
install -Dm644 packaging/systemd/%{name}.service %{buildroot}%{_userunitdir}/%{name}.service

# Desktop file (validated by desktop-file-install)
desktop-file-install \
    --dir=%{buildroot}%{_datadir}/applications \
    data/io.lamco.rdp-server.desktop

# AppStream metainfo
install -Dm644 data/io.lamco.rdp-server.metainfo.xml %{buildroot}%{_metainfodir}/io.lamco.rdp-server.metainfo.xml
install -Dm644 data/icons/io.lamco.rdp-server.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/io.lamco.rdp-server.svg
for size in 48 64 128 256; do
    install -Dm644 data/icons/io.lamco.rdp-server-${size}.png \
        %{buildroot}%{_datadir}/icons/hicolor/${size}x${size}/apps/io.lamco.rdp-server.png
done

%check
appstream-util validate-relax --nonet %{buildroot}%{_metainfodir}/io.lamco.rdp-server.metainfo.xml

%post
%systemd_user_post %{name}.service

%preun
%systemd_user_preun %{name}.service

%postun
%systemd_user_postun_with_restart %{name}.service

%files
%license LICENSE
%doc README.md
%{_bindir}/%{name}
%{_bindir}/%{name}-gui
%dir %{_sysconfdir}/%{name}
%{_userunitdir}/%{name}.service
%{_datadir}/applications/io.lamco.rdp-server.desktop
%{_metainfodir}/io.lamco.rdp-server.metainfo.xml
%{_datadir}/icons/hicolor/scalable/apps/io.lamco.rdp-server.svg
%{_datadir}/icons/hicolor/*/apps/io.lamco.rdp-server.png

%changelog
* Mon Feb 24 2026 Greg Lamberson <greg@lamco.io> - 1.4.0-1
- Clipboard provider trait rearchitecture with backend abstraction
  (Portal, Mutter D-Bus, wlr data-control providers)
- Enhanced wlroots compositor support via xdg-desktop-portal-generic
- MSRV raised to 1.88 (iced 0.14 requirement)
- Removed ironrdp-graphics vendor patch (cast_signed available at 1.88)

* Sun Feb 22 2026 Greg Lamberson <greg@lamco.io> - 1.3.1-5
- Use thin LTO and codegen-units=4 for distro builds to reduce peak memory
  from ~10GB to ~4GB (fixes OOM in mock on resource-constrained builders)
- Disable Fedora system LTO flags to prevent double-LTO interaction

* Sun Feb 22 2026 Greg Lamberson <greg@lamco.io> - 1.3.1-4
- Fix build with libva >= 2.22 (Fedora rawhide): add ..Default::default() to
  vendored cros-libva VP9 encoder struct for forward compatibility with new
  VAEncPictureParameterBufferVP9 fields (seg_id_block_size, va_reserved8)
- Apply patch at build time and clear vendor checksum for cros-libva

* Wed Feb 18 2026 Greg Lamberson <greg@lamco.io> - 1.3.1-3
- Use desktop-file-install for .desktop file per Fedora guidelines
- Validate metainfo.xml with appstream-util
- Add systemd user service scriptlets (post/preun/postun)
- Add BuildRequires: desktop-file-utils, libappstream-glib, systemd-rpm-macros

* Mon Feb 16 2026 Greg Lamberson <greg@lamco.io> - 1.3.1-2
- Add compound License tag enumerating all vendored crate licenses
- Add Provides: bundled(crate(...)) for 883 vendored Rust crates
- Add vendoring justification comment

* Sat Feb 14 2026 Greg Lamberson <greg@lamco.io> - 1.3.1-1
- Flathub packaging and metadata for Flatpak submission
- Clippy pedantic linting pass (deny-level pedantic warnings)
- iced 0.14 to 0.13 downgrade for distro Rust compatibility
- Rustfmt and editorconfig standardization
- Portal protocol compliance audit and roadmap
- OBS build procedure documentation and fixes

* Sat Feb 07 2026 Greg Lamberson <greg@lamco.io> - 1.3.0-1
- KDE Klipper clipboard cooperation mode (direct D-Bus integration)
- Session factory with automatic platform quirk detection
- EGFX reconnection fix (black screen on reconnect)
- Portal session crash fixes (session validity tracking)
- Graceful shutdown (Ctrl-C handler, explicit PipeWire shutdown)
- Flatpak log file creation fallback
- GUI reorganization (wired settings, server detach mode)

* Mon Jan 19 2026 Greg Lamberson <greg@lamco.io> - 1.0.0-1
- Major release with full-featured configuration GUI
- 10-tab graphical interface for all configuration options
- Professional dark theme with Lamco branding
- Server process management (start/stop/restart)
- TLS certificate generation wizard
- Live log viewer with filtering
- Real-time configuration validation
- Import/Export configuration files
- Hardware detection and capability display

* Sun Jan 18 2026 Greg Lamberson <greg@lamco.io> - 0.9.0-1
- Initial public release with core remote desktop functionality

* Wed Jan 14 2026 Greg Lamberson <greg@lamco.io> - 0.1.0-1
- Initial package
- RHEL 9 platform quirk detection (AVC444 disabled, clipboard unavailable)
- Multi-platform support via OBS
