# fbuild Development Guide

## Development Mode

When developing fbuild itself, you need to ensure that the development version uses separate directories from any production installation to avoid conflicts. fbuild provides a development mode that isolates:

1. **Daemon files** - PID, status, and request files
2. **Cache directories** - Downloaded packages, toolchains, and libraries

### Enabling Development Mode

Set the `FBUILD_DEV_MODE` environment variable before running fbuild commands:

```bash
# Linux/macOS
export FBUILD_DEV_MODE=1

# Windows (CMD)
set FBUILD_DEV_MODE=1

# Windows (PowerShell)
$env:FBUILD_DEV_MODE="1"
```

### What Changes in Development Mode

When `FBUILD_DEV_MODE=1` is set:

| Component | Production Location | Development Location |
|-----------|-------------------|---------------------|
| Daemon files | `~/.fbuild/daemon/` | `<repo>/.fbuild/daemon_dev/` |
| Cache files | `<project>/.fbuild/cache/` | `<project>/.fbuild/cache_dev/` |
| Build artifacts | `<project>/.fbuild/build/` | `<project>/.fbuild/build/` (unchanged) |

### Development Workflow

1. **Install in development mode:**
   ```bash
   pip install -e .
   ```

2. **Set development mode:**
   ```bash
   export FBUILD_DEV_MODE=1
   ```

3. **Run fbuild commands:**
   ```bash
   fbuild build tests/uno -e uno
   fbuild deploy tests/esp32c6 -e esp32c6
   ```

4. **Check daemon status:**
   ```bash
   fbuild daemon status
   ```

   The daemon will be running from `.fbuild/daemon_dev/` in the current directory.

### Why Development Mode is Needed

Without development mode, running fbuild from the repository would:

- Interfere with the production daemon in `~/.fbuild/daemon/`
- Potentially corrupt production cache in user projects
- Make it difficult to test daemon changes without affecting production

With development mode:

- Development daemon runs independently in `.fbuild/daemon_dev/`
- Development cache is isolated in `.fbuild/cache_dev/`
- Production fbuild installation remains unaffected
- Multiple developers can work on different branches without conflicts

### Cleaning Up Development Files

Development files are ignored by git (via `.gitignore`). To clean them manually:

```bash
# Remove development daemon files
rm -rf .fbuild/daemon_dev/

# Remove development cache
rm -rf .fbuild/cache_dev/

# Or remove all .fbuild directories
rm -rf .fbuild/
```

### Advanced Configuration

You can also set explicit paths for cache and daemon directories:

```bash
# Custom cache directory
export FBUILD_CACHE_DIR=/path/to/custom/cache

# Note: Daemon directory is controlled by FBUILD_DEV_MODE only
```

If `FBUILD_CACHE_DIR` is set, it overrides both production and development mode cache locations.

## Testing

When testing daemon functionality:

1. **Always use development mode** to avoid interfering with production
2. **Stop the development daemon** between test runs:
   ```bash
   fbuild daemon stop
   ```
3. **Clear daemon files** if needed:
   ```bash
   rm -rf .fbuild/daemon_dev/
   ```

## Troubleshooting

### Daemon not using development mode

Ensure:
- `FBUILD_DEV_MODE=1` is set in your environment
- You're running fbuild from the repository root
- The daemon was started after setting the environment variable

Check daemon location:
```bash
fbuild daemon status
# Look for daemon files in .fbuild/daemon_dev/
```

### Cache not isolated

Verify:
- `FBUILD_DEV_MODE=1` is set
- No `FBUILD_CACHE_DIR` override is set
- You're running builds from the correct directory

### Production daemon interfering

Stop the production daemon:
```bash
# Temporarily unset dev mode
unset FBUILD_DEV_MODE
fbuild daemon stop

# Re-enable dev mode
export FBUILD_DEV_MODE=1
```

## Contributing

When contributing to fbuild:

1. **Always develop with `FBUILD_DEV_MODE=1`**
2. **Test changes without affecting production**
3. **Document any new configuration options**
4. **Update this guide if adding new isolated directories**

## Environment Variable Reference

| Variable | Purpose | Default | Dev Mode |
|----------|---------|---------|----------|
| `FBUILD_DEV_MODE` | Enable development isolation | Not set (production) | `1` |
| `FBUILD_CACHE_DIR` | Override cache location | `<project>/.fbuild/cache/` | `<project>/.fbuild/cache_dev/` |

Setting `FBUILD_CACHE_DIR` overrides the dev mode cache location.
