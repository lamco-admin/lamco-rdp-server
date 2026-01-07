# Roadmap

This roadmap outlines planned development for lamco-rdp-server.

## Current Status: v0.1.0

Initial public release with core functionality:

- [x] XDG Portal screen capture (ScreenCast v4)
- [x] H.264 video streaming (EGFX pipeline)
- [x] AVC420 and AVC444 codec support
- [x] PipeWire video capture
- [x] Bidirectional clipboard (text, images, files)
- [x] libei input injection
- [x] PAM authentication
- [x] Session restore tokens
- [x] Multi-monitor support

## Near-Term (v0.2.x)

### Hardware Encoding

- [ ] VA-API support (Intel/AMD)
- [ ] NVENC support (NVIDIA)
- [ ] Encoder auto-selection based on hardware

### Audio Support

- [ ] PipeWire audio capture
- [ ] RDP audio channel (rdpsnd)
- [ ] Audio latency optimization

### Performance

- [ ] DMA-BUF zero-copy capture
- [ ] GPU-accelerated color conversion
- [ ] Improved damage tracking

## Medium-Term (v0.3.x)

### Multi-Monitor Enhancements

- [ ] Dynamic monitor add/remove
- [ ] Virtual monitor support
- [ ] Per-monitor quality settings

### Security

- [ ] NLA (Network Level Authentication)
- [ ] Certificate management UI
- [ ] Audit logging

### Protocol Extensions

- [ ] RemoteFX codec support
- [ ] Display Control Channel (resize)
- [ ] Graphics Pipeline improvements

## Long-Term

### Enterprise Features

- [ ] Session shadowing
- [ ] Multi-user support
- [ ] Load balancing integration

### Cross-Platform

- [ ] BSD support
- [ ] Container deployment
- [ ] Cloud integration

## Contributing

Contributions are welcome. Please open an issue to discuss significant changes before submitting a PR.

### Priority Areas

1. **Hardware encoding** - VA-API/NVENC would significantly improve performance
2. **Audio support** - Currently missing, high user demand
3. **Documentation** - API docs, deployment guides

### Testing Needed

- Different compositor configurations
- Various RDP clients (mstsc, FreeRDP, Remmina)
- Network conditions (latency, packet loss)
