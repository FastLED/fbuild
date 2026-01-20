"""
fbuild Fingerprint Cache System

Provides efficient change detection for build caching through file fingerprinting.
Migrated from FastLED CI system for use with fbuild.

Key features:
- SHA256-based file hashing for reliable change detection
- File locking for concurrent access safety
- Pre-computed fingerprint pattern for race-condition immunity
- Version-based cache invalidation
"""

from fbuild.fingerprint.core import (
    HashFingerprintCache,
    SourceFingerprintCache,
)

__all__ = [
    "HashFingerprintCache",
    "SourceFingerprintCache",
]
