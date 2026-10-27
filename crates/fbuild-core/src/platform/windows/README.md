# Windows implementation

Private Windows implementation details for `fbuild_core::platform`. Native
types must not escape through the neutral facade.

The filesystem implementation reads the fixed
`HKLM\SYSTEM\CurrentControlSet\Control\FileSystem\LongPathsEnabled` value.
Only a four-byte `REG_DWORD` equal to `1` is enabled. Missing values are
disabled; malformed values and native read failures remain errors. Repair
writes only that value and reads it back. The caller owns elevation and
restarting processes after repair; this module never requests UAC.

Legacy path lengths count UTF-16 code units without a trailing NUL. The short
cache base uses a validated local `SystemDrive` root and does not create any
directories or choose user/cache identities.

Automatic cache directories use a direct drive child and descendants beneath
it. `fs.rs` delegates their native mechanics to `private_directory.rs`.
`ensure_private_directory` installs a protected current-user-only DACL at
creation, verifies existing ownership before replacing the DACL, verifies the
result, and rejects files and reparse points. Existing foreign-owned objects
remain untouched. No-delete-sharing handles keep checked ancestors stable
through traversal; the drive-root ACL is never changed.
Concurrent verification retries only native sharing contention for up to five
seconds, releasing the entire attempt's handles before waiting.

`long-paths.manifest` is embedded by the CLI and daemon build scripts. It
declares `longPathAware` and `asInvoker`, so enabling the registry setting
does not cause ordinary builds to run elevated.
