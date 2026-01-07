# Logging Configuration

lamco-rdp-server uses `tracing` for structured logging with multiple output formats.

## Log Levels

| Level | When to Use | CLI Flag |
|-------|-------------|----------|
| error | Unrecoverable failures | (always on) |
| warn | Recoverable issues, degraded operation | (always on) |
| info | Connection events, state changes | default |
| debug | Protocol details, timing | `-v` |
| trace | Raw data, per-frame details | `-vv` |

## Command Line Options

```bash
# Default (info level)
lamco-rdp-server

# Debug level
lamco-rdp-server -v

# Trace level
lamco-rdp-server -vv

# JSON format (for log aggregation)
lamco-rdp-server --log-format json

# Compact format
lamco-rdp-server --log-format compact

# Write to file
lamco-rdp-server --log-file /var/log/lamco-rdp-server.log
```

## Environment Variables

Override defaults with `RUST_LOG`:

```bash
# Enable trace for clipboard debugging
RUST_LOG=lamco_rdp_server::clipboard=trace lamco-rdp-server

# Trace video pipeline
RUST_LOG=lamco_rdp_server::egfx=trace lamco-rdp-server

# Debug all lamco crates
RUST_LOG=lamco=debug lamco-rdp-server

# Debug IronRDP protocol (warning: verbose!)
RUST_LOG=ironrdp=debug lamco-rdp-server
```

## Log Categories

### Connection Events (info)
- Client connect/disconnect
- Authentication success/failure
- Session start/stop

### Video Pipeline (debug)
- Frame capture timing
- Encode statistics
- Bandwidth usage

### Clipboard (debug)
- Format negotiations
- Data transfers
- Echo suppression

### Input (trace)
- Key events
- Mouse movements
- Coordinate transformations

## Production Recommendations

For production deployments:

```toml
# config.toml
[logging]
level = "warn"          # Quiet by default
format = "json"         # Machine-parseable
file = "/var/log/lamco-rdp-server.log"
```

Use `RUST_LOG` environment variable for temporary debugging without restart:

```bash
# Temporarily enable debug logging
sudo systemctl set-environment RUST_LOG=lamco=debug
sudo systemctl restart lamco-rdp-server
```

## Log Rotation

When using `--log-file`, configure logrotate:

```
/var/log/lamco-rdp-server.log {
    daily
    rotate 7
    compress
    delaycompress
    missingok
    notifempty
    create 640 root adm
    postrotate
        systemctl kill -s USR1 lamco-rdp-server
    endscript
}
```
