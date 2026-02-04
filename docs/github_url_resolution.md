# GitHub URL Resolution for PlatformIO Platforms

This document describes the GitHub URL transformation utilities that enable fbuild to handle various platform specification formats and automatically convert them to canonical download URLs.

## Problem Statement

GitHub provides multiple ways to access repository content:
1. **Release assets** (`/releases/download/v1.0.0/file.zip`) - Often return 404 errors if not explicitly created
2. **Git URLs** (`.git` suffix) - Require git clone, which is slow and resource-intensive
3. **Branch/tag URLs** (`/tree/branch`) - Not direct download links

PlatformIO uses abbreviated platform specifications like `espressif8266@4.2.1` which need to be resolved to actual download URLs.

## Solution

The `github_url_utils.py` module provides utilities to:
1. Transform any GitHub URL format into canonical archive download URLs
2. Resolve PlatformIO platform specifications to download URLs
3. Verify URLs exist before attempting downloads
4. Handle GitHub's archive format (strip top-level directories)

## API Reference

### GitHub URL Transformation

#### `transform_github_url(url: str, prefer_zip: bool = True) -> str`

Transforms various GitHub URL formats into direct archive download URLs.

**Supported formats:**
```python
# Simple repo
"https://github.com/owner/repo"
→ "https://github.com/owner/repo/archive/refs/heads/main.zip"

# Git URL
"https://github.com/owner/repo.git"
→ "https://github.com/owner/repo/archive/refs/heads/main.zip"

# Branch
"https://github.com/owner/repo/tree/develop"
→ "https://github.com/owner/repo/archive/refs/heads/develop.zip"

# Tag (with 'v' prefix)
"https://github.com/owner/repo/tree/v1.0.0"
→ "https://github.com/owner/repo/archive/refs/tags/v1.0.0.zip"

# Commit
"https://github.com/owner/repo/commit/abc123"
→ "https://github.com/owner/repo/archive/abc123.zip"

# Release download (problematic format)
"https://github.com/owner/repo/releases/download/v1.0.0/file.zip"
→ "https://github.com/owner/repo/archive/refs/tags/v1.0.0.zip"
```

#### `verify_github_url(url: str, timeout: int = 10) -> Tuple[bool, Optional[str]]`

Verifies a GitHub archive URL exists using HTTP HEAD request.

**Returns:** `(exists: bool, final_url: Optional[str])`

**Example:**
```python
exists, final_url = verify_github_url(
    "https://github.com/platformio/platform-espressif8266/archive/refs/tags/v4.2.1.zip"
)
# exists = True
# final_url = "https://codeload.github.com/platformio/platform-espressif8266/zip/refs/tags/v4.2.1"
```

#### `transform_and_verify_github_url(...) -> Tuple[str, bool, Optional[str]]`

Convenience function combining transformation and verification.

**Returns:** `(transformed_url: str, exists: bool, final_url: Optional[str])`

### PlatformIO Platform Resolution

#### `resolve_platformio_platform_url(platform_spec: str, prefer_zip: bool = True) -> str`

Resolves PlatformIO platform specifications to canonical download URLs.

**Supported formats:**

| Format | Example | Resolved URL |
|--------|---------|--------------|
| Platform name | `espressif8266` | `https://github.com/platformio/platform-espressif8266/archive/refs/heads/master.zip` |
| Platform + version | `espressif8266@4.2.1` | `https://github.com/platformio/platform-espressif8266/archive/refs/tags/v4.2.1.zip` |
| Platform + version (with v) | `espressif8266@v4.2.1` | `https://github.com/platformio/platform-espressif8266/archive/refs/tags/v4.2.1.zip` |
| Owner + platform | `platformio/espressif8266` | `https://github.com/platformio/platform-espressif8266/archive/refs/heads/master.zip` |
| Owner + platform + version | `platformio/espressif8266@4.2.1` | `https://github.com/platformio/platform-espressif8266/archive/refs/tags/v4.2.1.zip` |
| Custom owner | `myuser/espressif8266@1.0.0` | `https://github.com/myuser/platform-espressif8266/archive/refs/tags/v1.0.0.zip` |
| Full GitHub URL | `https://github.com/...` | Passed through `transform_github_url()` |

**Example:**
```python
from fbuild.packages.github_url_utils import resolve_platformio_platform_url

# All these produce the same URL:
specs = [
    "espressif8266@4.2.1",
    "espressif8266@v4.2.1",
    "platformio/espressif8266@4.2.1",
    "https://github.com/platformio/platform-espressif8266/tree/v4.2.1",
    "https://github.com/platformio/platform-espressif8266/releases/download/v4.2.1/file.zip",
]

for spec in specs:
    url = resolve_platformio_platform_url(spec)
    # url = "https://github.com/platformio/platform-espressif8266/archive/refs/tags/v4.2.1.zip"
```

#### `resolve_and_verify_platformio_url(...) -> Tuple[str, bool, Optional[str]]`

Resolves and verifies a PlatformIO platform specification.

**Returns:** `(resolved_url: str, exists: bool, final_url: Optional[str])`

## Usage in platformio.ini

You can now use abbreviated platform specifications in your `platformio.ini`:

```ini
[env:esp8266]
# Abbreviated format (recommended)
platform = espressif8266@4.2.1
board = nodemcuv2
framework = arduino

[env:esp32]
# With owner
platform = platformio/espressif32@6.5.0
board = esp32dev
framework = arduino

[env:custom]
# Custom owner
platform = myuser/custom-platform@1.0.0
board = custom_board
framework = arduino

[env:latest]
# Latest version (master branch)
platform = espressif8266
board = nodemcuv2
framework = arduino

[env:full_url]
# Full URL still works
platform = https://github.com/platformio/platform-espressif8266/tree/v4.2.1
board = nodemcuv2
framework = arduino
```

## Archive Extraction

The downloader automatically handles GitHub's archive format which includes a top-level directory:

```
# GitHub archive structure:
platform-espressif8266-4.2.1/
├── boards/
├── platform.json
└── ...

# After extraction (automatic stripping):
cache/platforms/{hash}/
├── boards/
├── platform.json
└── ...
```

The `_extract_zip()` and `_extract_tar()` methods in `downloader.py` automatically detect and strip single top-level directories.

## Test Coverage

**File:** `tests/unit/packages/test_github_url_utils.py`

- ✅ 43 tests total
- ✅ GitHub URL transformation (14 tests)
- ✅ URL verification with HTTP HEAD (3 tests)
- ✅ Combined transform + verify (3 tests)
- ✅ PlatformIO platform resolution (17 tests)
- ✅ Integration tests (3 tests)
- ✅ Real-world examples (3 tests)

Run tests:
```bash
pytest tests/unit/packages/test_github_url_utils.py -v
```

## Benefits

1. **✅ No more 404 errors** - Uses GitHub's reliable archive API instead of release assets
2. **✅ Faster downloads** - Direct archive downloads instead of git clone
3. **✅ Simpler configuration** - Use abbreviated `platform@version` format
4. **✅ Automatic verification** - HTTP HEAD checks before downloading
5. **✅ Consistent URLs** - All formats resolve to canonical archive URLs
6. **✅ Cross-platform** - Works on Windows, macOS, and Linux

## Implementation Details

### URL Resolution Flow

```
platformio.ini:
  platform = espressif8266@4.2.1

      ↓

OrchestratorESP8266._resolve_platform_url()
  → resolve_platformio_platform_url("espressif8266@4.2.1")

      ↓

GitHub canonical URL:
  https://github.com/platformio/platform-espressif8266/archive/refs/tags/v4.2.1.zip

      ↓

PackageDownloader.download_and_extract()
  → Downloads to cache
  → Extracts and strips top-level directory

      ↓

Platform ready for use:
  ~/.fbuild/cache/platforms/{hash}/
```

### Version Prefix Handling

The resolver automatically adds 'v' prefix to version numbers that start with digits:

```python
"espressif8266@4.2.1"  → ".../v4.2.1.zip"  # 'v' added
"espressif8266@v4.2.1" → ".../v4.2.1.zip"  # 'v' preserved
"espressif8266@beta1"  → ".../beta1.zip"   # No 'v' added
```

### Platform Name Handling

The resolver automatically adds 'platform-' prefix if not present:

```python
"espressif8266@4.2.1"          → "platform-espressif8266"
"platform-espressif8266@4.2.1" → "platform-espressif8266" (unchanged)
```

## Migration Guide

### Before (fbuild v1.4.2 and earlier)

```ini
[env:esp8266]
# Had to use full GitHub URL
platform = https://github.com/platformio/platform-espressif8266/releases/download/v4.2.1/platform-espressif8266.zip
board = nodemcuv2
framework = arduino
```

Problem: Release download URLs often returned 404 errors because GitHub doesn't automatically create release assets.

### After (fbuild v1.4.3+)

```ini
[env:esp8266]
# Use abbreviated format
platform = espressif8266@4.2.1
board = nodemcuv2
framework = arduino
```

Benefits:
- ✅ Shorter, more readable
- ✅ Matches PlatformIO's official format
- ✅ Automatically resolves to reliable archive URL
- ✅ Verified to exist before downloading

## Error Handling

### Invalid Platform Specifications

```python
from fbuild.packages.github_url_utils import PlatformIOURLError

try:
    url = resolve_platformio_platform_url("invalid spec with spaces")
except PlatformIOURLError as e:
    print(f"Invalid specification: {e}")
    # Invalid specification: Invalid platform specification: invalid spec with spaces
```

### Non-existent Versions

```python
url, exists, final = resolve_and_verify_platformio_url("espressif8266@999.999.999")
# url = "https://github.com/platformio/platform-espressif8266/archive/refs/tags/v999.999.999.zip"
# exists = False
# final = None
```

## Future Enhancements

Potential improvements for future versions:

1. **Registry API integration** - Query PlatformIO registry for available versions
2. **Version constraints** - Support `@^4.0.0` or `@>=4.2.0` syntax
3. **Caching** - Cache resolved URLs to avoid repeated HTTP HEAD requests
4. **Parallel verification** - Verify multiple versions concurrently
5. **Auto-update** - Check for newer versions and suggest updates

## References

- [PlatformIO Platforms](https://docs.platformio.org/en/latest/platforms/index.html)
- [PlatformIO Registry](https://registry.platformio.org/)
- [GitHub Archive API](https://docs.github.com/en/repositories/working-with-files/using-files/downloading-source-code-archives)
- [ESP8266 Platform Repository](https://github.com/platformio/platform-espressif8266)
