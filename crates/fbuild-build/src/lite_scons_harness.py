#!/usr/bin/env python3
"""Lite-SCons harness (FastLED/fbuild#553).

A subset-of-SCons interpreter for PlatformIO `extra_scripts`. Acts as an
opt-in companion to `script_runtime_harness.py` (the MockEnv shim) for
projects that need the SCons primitives MockEnv structurally can't model:

  * effectful `env.Execute(env.Action(callable_or_cmd))` — generator scripts
  * `env.AddPreAction` / `env.AddPostAction` recorded with the unresolved
    target template so fbuild can subst + invoke at the right time
  * `env.AddBuildMiddleware(callback, regex)` recorded for per-source
    pre-compile hooks
  * `env.AddCustomTarget(...)` recorded
  * recursive `env.SConscript("child.py")` chained mutation
  * `env.AddMethod(callable, name)` for script-defined helpers
  * `env.ParseFlagsExtended(line)` routing both `-Ipath` and `-I path` forms

INPUT (sys.argv[1]) — matches `script_runtime_harness.py`:
    {
      "project_dir": "...",
      "env_name": "...",
      "extra_scripts": ["pre:gen.py", "post:merge.py"],
      "project_options": {...},
      "board_config": {...},
      "platform_name": "...",
      "platformio_home": "..."
    }

OUTPUT (stdout) — extends the existing shape:
    {
      "env":     {<existing scope state>},
      "projenv": {<existing scope state>},
      "notes":        [...],
      "unsupported":  [...],
      "lite_scons_records": {
        "executed_actions": [{"kind": "callable"|"command", "repr": "...", "rc": 0,
                              "stdout": "...", "stderr": "...", "wallclock_ms": 12}],
        "generated_files":  [{"path": "...", "size": 123, "mtime_ns": ...}],
        "recorded_pre_actions":  [{"target": "$BUILD_DIR/...", "action_repr": "..."}],
        "recorded_post_actions": [{"target": "$BUILD_DIR/...", "action_repr": "..."}],
        "custom_targets":   [{"name": "...", "deps": [...], "actions": [...], "kwargs": {...}}],
        "middleware":       [{"callback_repr": "...", "regex": "..."}],
        "builder_calls":    [{"builder": "...", "target": "...", "sources": [...]}]
      }
    }

NOT a full SCons. Three boundaries the production design intentionally
keeps:

  1. NO DAG / incremental rebuilds. Single-pass resolve-then-return.
  2. NO scanner-driven header dep discovery (fbuild has its own).
  3. NO PlatformIO-defined chip-family builders (`env.MergeFlashImage`,
     `env.PackageJsonFirmware`, etc.). Those are recorded as
     `builder_calls` entries; the Rust side either maps them to native
     fbuild-deploy operations or fails fast with a structured "needs
     `--platformio` for builder X" message.

See https://github.com/FastLED/fbuild/issues/553#issuecomment-4702659508
for the spike that vetted this surface area against the documented #43
real-world repo sample.
"""

from __future__ import annotations

import contextlib
import io
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import traceback
import types


# --------------------------------------------------------------------------
# Scope tables (carry the existing MockEnv contract forward 1:1 so the
# Rust-side BuildOverlay decoder can consume either harness's output).
# --------------------------------------------------------------------------

SUPPORTED_SCOPES = {
    "CPPDEFINES", "CPPPATH", "CCFLAGS", "CFLAGS", "CXXFLAGS",
    "ASFLAGS", "LINKFLAGS", "LIBPATH", "LIBS",
}
MUTABLE_SCOPES = SUPPORTED_SCOPES | {"BUILD_FLAGS"}

KNOWN_VAR_SCOPES = {
    "PROGNAME", "PROGSUFFIX", "BUILD_DIR",
    "MKSPIFFSTOOL", "MKFSTOOL",
    "UPLOAD_PROTOCOL", "UPLOADER", "UPLOADCMD",
    "PIOENV", "PROJECT_DIR", "PROJECT_SRC_DIR", "PROJECT_BUILD_DIR",
    "ESPTOOL", "PIOPLATFORM",
}


class RuntimeFailure(Exception):
    pass


# --------------------------------------------------------------------------
# Core SCons primitives
# --------------------------------------------------------------------------


class Action:
    """SCons Action — wraps a command string OR a Python callable.

    Real SCons Action is a polymorphic factory. The lite version supports
    only the two shapes PlatformIO scripts actually use:
        Action("$CC $CCFLAGS $SOURCES -o $TARGET", "Compiling $TARGET")
        Action(some_python_callable, "Generating headers")
    """

    def __init__(self, action, strfunction=None):
        self.action = action
        self.strfunction = strfunction

    def __repr__(self):
        if callable(self.action):
            nm = getattr(self.action, "__name__", repr(self.action))
            return f"<Action callable={nm}>"
        return f"<Action cmd={self.action!r}>"

    def execute(self, env, target=None, source=None):
        started = time.monotonic()
        if callable(self.action):
            try:
                ret = self.action(target=target, source=source, env=env)
                rc = 0 if (ret is None or ret == 0) else int(ret)
                return {
                    "kind": "callable", "repr": repr(self), "rc": rc,
                    "stdout": "", "stderr": "",
                    "wallclock_ms": int((time.monotonic() - started) * 1000),
                }
            except Exception as exc:
                return {
                    "kind": "callable", "repr": repr(self), "rc": 1,
                    "stdout": "",
                    "stderr": f"{type(exc).__name__}: {exc}\n{traceback.format_exc()}",
                    "wallclock_ms": int((time.monotonic() - started) * 1000),
                }
        cmd = env.subst(str(self.action))
        try:
            proc = subprocess.run(
                cmd, shell=True, capture_output=True, text=True,
                cwd=env._vars.get("PROJECT_DIR"), timeout=120,
            )
            return {
                "kind": "command", "repr": cmd, "rc": proc.returncode,
                "stdout": proc.stdout[-4096:], "stderr": proc.stderr[-4096:],
                "wallclock_ms": int((time.monotonic() - started) * 1000),
            }
        except subprocess.TimeoutExpired:
            return {
                "kind": "command", "repr": cmd, "rc": 124,
                "stdout": "", "stderr": "timed out after 120s",
                "wallclock_ms": int((time.monotonic() - started) * 1000),
            }


class Builder:
    """SCons Builder. Scripts register builders via
    `env.Append(BUILDERS={"Foo": Builder(...)})` or `env.AddMethod`.
    The lite harness records each builder invocation as a `builder_calls`
    entry — fbuild maps known names to native operations or fails fast."""

    def __init__(self, action=None, src_suffix=None, suffix=None, **kwargs):
        self.action = action
        self.src_suffix = src_suffix
        self.suffix = suffix
        self.kwargs = kwargs

    def __repr__(self):
        return f"<Builder action={self.action!r} suffix={self.suffix!r}>"


class Node:
    """Path wrapper with `__str__` returning the path. Enough for `env.File()`
    / `env.Dir()` callers; we don't model SCons's File/Dir DAG."""

    def __init__(self, path, is_dir=False):
        self.path = path
        self.is_dir = is_dir

    def __str__(self): return self.path
    def __repr__(self): return f"<{'Dir' if self.is_dir else 'File'} {self.path!r}>"
    def __fspath__(self): return self.path


# --------------------------------------------------------------------------
# Environment
# --------------------------------------------------------------------------


_SUBST_RE = re.compile(r"\$(?:\{([A-Za-z_][A-Za-z0-9_]*)\}|([A-Za-z_][A-Za-z0-9_]*))")


class Environment:
    """The construction environment scripts mutate.

    Two flavours mirror the existing MockEnv contract: `env` (global) and
    `projenv` (project-only). Real SCons keeps these as separate
    construction environments; the lite harness keeps a separate flag-scope
    state per Environment object but shares the same Ledger so all recorded
    actions/middleware/builder_calls land in one output bucket.
    """

    def __init__(self, label, project_dir, env_name, project_options,
                 board_config, platform_name, platformio_home, ledger):
        self._label = label
        self._project_dir = project_dir
        self._env_name = env_name
        self._project_options = project_options
        self._board_config = board_config
        self._platform_name = platform_name
        self._platformio_home = platformio_home
        self._ledger = ledger
        self._scopes = {k: [] for k in MUTABLE_SCOPES}
        self._vars = {
            "PROJECT_DIR": project_dir,
            "PROJECT_SRC_DIR": os.path.join(project_dir, "src"),
            "PROJECT_BUILD_DIR": os.path.join(project_dir, ".pio", "build"),
            "BUILD_DIR": os.path.join(project_dir, ".pio", "build", env_name),
            "PIOENV": env_name,
            "PIOPLATFORM": platform_name or "",
            "PROGNAME": "firmware",
            "PROGSUFFIX": ".elf",
        }
        self._methods = {}
        self._pre_actions = {}
        self._post_actions = {}

    # ----- subst -----------------------------------------------------

    def subst(self, text, max_depth=8):
        if not isinstance(text, str): return text
        last, cur, depth = None, text, 0
        while cur != last and depth < max_depth:
            last = cur
            cur = _SUBST_RE.sub(self._subst_one, cur)
            depth += 1
        return cur

    def _subst_one(self, m):
        name = m.group(1) or m.group(2)
        if name in self._vars:
            v = self._vars[name]
            return " ".join(str(x) for x in v) if isinstance(v, list) else str(v)
        return ""

    # ----- flag-scope normalisation (mirrors existing MockEnv shape) ----

    def _normalize_define(self, value):
        if isinstance(value, (list, tuple)):
            if len(value) == 2 and not isinstance(value[0], (list, tuple, dict)):
                return [{"kind": "kv", "key": str(value[0]), "value": value[1]}]
            out = []
            for v in value: out.extend(self._normalize_define(v))
            return out
        if isinstance(value, dict): return [value]
        return [str(value)]

    def _normalize_path(self, value):
        if value is None: return None
        r = self.subst(str(value))
        if not os.path.isabs(r): r = os.path.join(self._project_dir, r)
        return {"kind": "path", "value": r}

    def _normalize(self, scope, value):
        if scope == "CPPDEFINES": return self._normalize_define(value)
        if scope in ("CPPPATH", "LIBPATH"):
            if isinstance(value, (list, tuple)):
                return [self._normalize_path(v) for v in value]
            return [self._normalize_path(value)]
        if isinstance(value, (list, tuple)):
            return [str(v) for v in value]
        return [str(value)]

    def _mutate(self, mode, kwargs):
        for scope, value in kwargs.items():
            if scope == "BUILDERS":
                if isinstance(value, dict):
                    for name, builder in value.items():
                        # Closure-capture both builder and its registered name.
                        self._methods[name] = lambda *a, _b=builder, _n=name, **kw: self._record_builder_call(_b, _n, a, kw)
                continue
            if scope not in MUTABLE_SCOPES and scope not in KNOWN_VAR_SCOPES:
                # Permissive: stash in _vars so subsequent reads work.
                if mode == "replace":
                    self._vars[scope] = value
                else:
                    existing = self._vars.get(scope)
                    if isinstance(existing, list):
                        new = value if isinstance(value, list) else [value]
                        if mode == "append":         existing.extend(new)
                        elif mode == "append_unique":
                            for v in new:
                                if v not in existing: existing.append(v)
                        else:                         existing[:0] = new
                    else:
                        self._vars[scope] = value
                continue
            if scope in KNOWN_VAR_SCOPES and scope not in MUTABLE_SCOPES:
                self._vars[scope] = value
                continue
            items = self._normalize(scope, value)
            current = self._scopes[scope]
            if mode == "replace": self._scopes[scope] = items
            elif mode == "append": current.extend(items)
            elif mode == "append_unique":
                for it in items:
                    if it not in current: current.append(it)
            elif mode == "prepend": self._scopes[scope] = items + current

    def _record_builder_call(self, builder, name, args, kwargs):
        target = args[0] if args else kwargs.get("target", f"{name}_target")
        sources = args[1] if len(args) > 1 else kwargs.get("source", [])
        srcs = sources if isinstance(sources, list) else [sources]
        self._ledger.builder_calls.append({
            "builder": name, "target": str(target),
            "sources": [str(s) for s in srcs],
        })
        return Node(str(target))

    # ----- script-facing API -----------------------------------------

    def Append(self, **kwargs):       self._mutate("append", kwargs)
    def AppendUnique(self, **kwargs): self._mutate("append_unique", kwargs)
    def Prepend(self, **kwargs):      self._mutate("prepend", kwargs)
    def Replace(self, **kwargs):      self._mutate("replace", kwargs)

    def get(self, key, default=None):
        if key in self._vars: return self._vars[key]
        if key in self._scopes: return self._scopes[key]
        # Scripts treat env.get(K) and GetProjectOption(K) as interchangeable
        # (#553 spike caught this — bug 1 of 3).
        if key in self._project_options:
            return self._project_options[key]
        return default

    def Dump(self): return dict(self._vars, **self._scopes)
    def Action(self, *args, **kwargs): return Action(*args, **kwargs)

    def VerboseAction(self, action, message=None):
        if not isinstance(action, Action):
            action = Action(action, message)
        return action

    def Execute(self, action):
        if not isinstance(action, Action): action = Action(action)
        rec = action.execute(self)
        self._ledger.executed_actions.append(rec)
        return rec["rc"]

    def AddPreAction(self, target, action):
        if not isinstance(action, Action): action = Action(action)
        self._pre_actions.setdefault(str(target), []).append(action)
        self._ledger.recorded_pre_actions.append({
            "target": str(target), "action_repr": repr(action),
        })

    def AddPostAction(self, target, action):
        if not isinstance(action, Action): action = Action(action)
        self._post_actions.setdefault(str(target), []).append(action)
        self._ledger.recorded_post_actions.append({
            "target": str(target), "action_repr": repr(action),
        })

    def AddCustomTarget(self, name, dependencies=None, actions=None, **kwargs):
        action_reprs = []
        if actions:
            for a in (actions if isinstance(actions, list) else [actions]):
                if not isinstance(a, Action): a = Action(a)
                action_reprs.append(repr(a))
        self._ledger.custom_targets.append({
            "name": name,
            "deps": [str(d) for d in (dependencies or [])],
            "actions": action_reprs,
            "kwargs": {k: str(v) for k, v in kwargs.items()},
        })

    def AddBuildMiddleware(self, callback, regex=None):
        self._ledger.middleware.append({
            "callback_repr": getattr(callback, "__name__", repr(callback)),
            "regex": regex,
        })

    def AddMethod(self, callable_obj, name=None):
        nm = name or callable_obj.__name__
        self._methods[nm] = callable_obj

    def ParseFlagsExtended(self, flag_str):
        """Parse a PlatformIO flag string into per-scope buckets.

        Handles both `-Ipath` (joined) and `-I path` (space-separated) forms
        for `-I` / `-L` / `-l`. The #553 spike caught the space-form variant
        (bug 2 of 3); real SCons handles both.
        """
        out = {"CPPDEFINES": [], "CPPPATH": [], "CCFLAGS": [],
               "CXXFLAGS": [], "LINKFLAGS": [], "LIBPATH": [], "LIBS": []}
        tokens = flag_str.split()
        i = 0
        while i < len(tokens):
            t = tokens[i]
            if t.startswith("-D"):
                arg = t[2:]
                if "=" in arg:
                    k, v = arg.split("=", 1)
                    out["CPPDEFINES"].append((k, v))
                else:
                    out["CPPDEFINES"].append(arg)
            elif t.startswith("-I"):
                arg = t[2:] if len(t) > 2 else (tokens[i + 1] if i + 1 < len(tokens) else "")
                if len(t) == 2: i += 1
                if arg: out["CPPPATH"].append(arg)
            elif t.startswith("-L"):
                arg = t[2:] if len(t) > 2 else (tokens[i + 1] if i + 1 < len(tokens) else "")
                if len(t) == 2: i += 1
                if arg: out["LIBPATH"].append(arg)
            elif t.startswith("-l"):
                arg = t[2:] if len(t) > 2 else (tokens[i + 1] if i + 1 < len(tokens) else "")
                if len(t) == 2: i += 1
                if arg: out["LIBS"].append(arg)
            elif t.startswith("-Wl,"):
                out["LINKFLAGS"].append(t)
            else:
                out["CCFLAGS"].append(t)
            i += 1
        return out

    def File(self, path): return Node(self.subst(str(path)))
    def Dir(self, path):  return Node(self.subst(str(path)), is_dir=True)
    def Flatten(self, seq):
        out = []
        def walk(x):
            if isinstance(x, (list, tuple)):
                for v in x: walk(v)
            else: out.append(x)
        walk(seq)
        return out

    def IsCleanTarget(self): return False
    def IsIntegrationDump(self): return False
    def GetBuildType(self):     return self._project_options.get("build_type", "release")
    def GetProjectOption(self, key, default=None): return self._project_options.get(key, default)
    def GetProjectOptions(self): return list(self._project_options.items())

    def GetProjectConfig(self):
        class _Cfg:
            def __init__(self, opts): self._opts = opts
            def get(self, section, key, fallback=None): return self._opts.get(key, fallback)
            def has_option(self, section, key): return key in self._opts
        return _Cfg(self._project_options)

    def BoardConfig(self): return _BoardConfig(self._board_config)
    def PioPlatform(self): return _PioPlatform(self._platform_name, self._platformio_home)

    def SConscript(self, path, exports=None):
        """Recursively eval a SCons fragment. Real SCons resolves the
        path relative to the CALLING script's directory (#553 spike caught
        this — bug 3 of 3); we track the current script dir on `_vars` and
        restore it after."""
        if os.path.isabs(path):
            full = path
        else:
            caller_dir = self._vars.get("__CURRENT_SCRIPT_DIR__") or self._project_dir
            full = os.path.join(caller_dir, path)
        if not os.path.exists(full):
            self._ledger.notes.append(f"SConscript missing: {full}")
            return None
        prev_dir = self._vars.get("__CURRENT_SCRIPT_DIR__")
        self._vars["__CURRENT_SCRIPT_DIR__"] = os.path.dirname(full)
        scope = {
            "__file__": full, "__name__": "__main__",
            "env": self,
            "Import": lambda *names: None,
            "DefaultEnvironment": lambda: self,
        }
        if exports: scope.update(exports)
        try:
            with open(full, "r", encoding="utf-8") as fh:
                exec(compile(fh.read(), full, "exec"), scope)
        except Exception as exc:
            self._ledger.errors.append(f"SConscript {full} raised: {exc}")
        finally:
            if prev_dir is None:
                self._vars.pop("__CURRENT_SCRIPT_DIR__", None)
            else:
                self._vars["__CURRENT_SCRIPT_DIR__"] = prev_dir

    def Clone(self, **kwargs): return self  # lite scope — no per-clone isolation

    def __getitem__(self, key):
        if key in self._vars: return self._vars[key]
        if key in self._scopes: return self._scopes[key]
        raise KeyError(key)

    def __setitem__(self, key, value):
        if key in MUTABLE_SCOPES:
            self._scopes[key] = self._normalize(key, value)
        else:
            self._vars[key] = value

    def __contains__(self, key):
        return key in self._vars or key in self._scopes

    def __getattr__(self, name):
        # Dynamically-bound methods take precedence (AddMethod + BUILDERS).
        if name in self.__dict__.get("_methods", {}):
            method = self.__dict__["_methods"][name]
            return lambda *a, **kw: method(self, *a, **kw)
        if name.startswith("_"):
            raise AttributeError(name)
        # Permissive default: record + no-op. Matches the MockEnv's
        # NOOP_METHODS philosophy but extends to anything not explicitly
        # implemented. The Rust side surfaces these in `notes`.
        ledger = self.__dict__.get("_ledger")
        if ledger is not None:
            ledger.notes.append(f"{self._label}.{name} returned no-op (unknown method)")
        return lambda *a, **kw: None

    def export_state(self):
        """Mirror MockEnv.export_state — BUILD_FLAGS folds into CCFLAGS."""
        ccflags = [str(item) for item in self._scopes["BUILD_FLAGS"]]
        ccflags.extend(self._scopes["CCFLAGS"])
        out = {
            "cppdefines": [],
            "cpppath":    self._scopes["CPPPATH"],
            "ccflags":    ccflags,
            "cflags":     self._scopes["CFLAGS"],
            "cxxflags":   self._scopes["CXXFLAGS"],
            "asflags":    self._scopes["ASFLAGS"],
            "linkflags":  self._scopes["LINKFLAGS"],
            "libpath":    self._scopes["LIBPATH"],
            "libs":       self._scopes["LIBS"],
        }
        # CPPDEFINES re-normalise for in-place tuple appends — same fix as MockEnv.
        for entry in self._scopes["CPPDEFINES"]:
            out["cppdefines"].extend(self._normalize_define(entry))
        return out


class _BoardConfig:
    def __init__(self, data): self._data = data
    def get(self, key, default=None):
        # fbuild's Rust side hands us a flat dict whose keys are already
        # dotted (e.g. {"build.mcu": "atmega328p"}), matching the existing
        # MockEnv contract. Real-SCons / PlatformIO scripts call
        # `BoardConfig.get("build.mcu")` against that flat dict directly,
        # so a flat lookup wins. Fall back to a nested walk only if the
        # caller did pass a nested dict (defensive — keeps the lite path
        # working if future board_config shapes change).
        if isinstance(self._data, dict) and key in self._data:
            return self._data[key]
        ref = self._data
        for part in str(key).split("."):
            if isinstance(ref, dict) and part in ref: ref = ref[part]
            else: return default
        return ref
    def __getitem__(self, key):
        v = self.get(key, None)
        if v is None: raise KeyError(key)
        return v


class _PioPlatform:
    def __init__(self, name, home): self.name, self._home = name or "", home or ""
    def is_embedded(self): return True
    def get_package_dir(self, package): return os.path.join(self._home, "packages", package)


# --------------------------------------------------------------------------
# Ledger — captures everything fbuild needs to replay
# --------------------------------------------------------------------------


class Ledger:
    def __init__(self):
        self.executed_actions = []
        self.recorded_pre_actions = []
        self.recorded_post_actions = []
        self.custom_targets = []
        self.middleware = []
        self.builder_calls = []
        self.notes = []
        self.errors = []
        self._mtimes_before = {}

    def snapshot_dir(self, root):
        for dp, _, files in os.walk(root):
            for fn in files:
                p = os.path.join(dp, fn)
                try:
                    self._mtimes_before[p] = os.path.getmtime(p)
                except OSError:
                    pass

    def newly_generated(self, root):
        out = []
        for dp, _, files in os.walk(root):
            for fn in files:
                p = os.path.join(dp, fn)
                try:
                    mtime = os.path.getmtime(p)
                except OSError:
                    continue
                before = self._mtimes_before.get(p)
                if before is None or before < mtime:
                    try:
                        sz = os.path.getsize(p)
                    except OSError:
                        sz = 0
                    out.append({"path": p, "mtime_ns": int(mtime * 1e9), "size": sz})
        return out


# --------------------------------------------------------------------------
# SCons.Script module shim — matches existing MockEnv harness shape
# --------------------------------------------------------------------------


def install_scons_module(env):
    scons = sys.modules.get("SCons") or types.ModuleType("SCons")
    script = sys.modules.get("SCons.Script") or types.ModuleType("SCons.Script")

    def Import(*names):
        import inspect
        frame = inspect.currentframe().f_back
        if frame is None:
            return
        for nm in names:
            if nm in ("env", "projenv", "DefaultEnvironment"):
                frame.f_globals[nm] = env

    setattr(script, "Import", Import)
    setattr(script, "DefaultEnvironment", lambda *a, **kw: env)
    setattr(script, "COMMAND_LINE_TARGETS", [])
    setattr(script, "ARGUMENTS", {})
    setattr(script, "Builder", Builder)
    setattr(script, "Action", Action)
    setattr(scons, "Script", script)
    sys.modules["SCons"] = scons
    sys.modules["SCons.Script"] = script


def resolve_script_entry(env, raw):
    """Parse `pre:/post:` prefix from an extra_scripts entry — matches the
    existing MockEnv harness contract."""
    if ":" in raw:
        prefix, path = raw.split(":", 1)
        if prefix not in ("pre", "post"):
            raise RuntimeFailure(f"unsupported extra_scripts prefix '{prefix}'")
        return prefix, os.path.abspath(env.subst(path))
    return "post", os.path.abspath(env.subst(raw))


def run_script(env, script_path):
    script_dir = os.path.dirname(script_path)
    old_path = list(sys.path)
    prev_dir = env._vars.get("__CURRENT_SCRIPT_DIR__")
    env._vars["__CURRENT_SCRIPT_DIR__"] = script_dir
    sys.path.insert(0, script_dir)
    sys.path.insert(0, env._vars["PROJECT_DIR"])
    try:
        scope = {
            "__file__": script_path, "__name__": "__main__",
            "env": env,
            "Import": sys.modules["SCons.Script"].Import,
            "DefaultEnvironment": lambda *a, **kw: env,
            "Builder": Builder, "Action": Action,
            "COMMAND_LINE_TARGETS": [], "ARGUMENTS": {},
        }
        with open(script_path, "r", encoding="utf-8") as fh:
            exec(compile(fh.read(), script_path, "exec"), scope)
    finally:
        sys.path[:] = old_path
        if prev_dir is None:
            env._vars.pop("__CURRENT_SCRIPT_DIR__", None)
        else:
            env._vars["__CURRENT_SCRIPT_DIR__"] = prev_dir


def run_script_captured(env, ledger, path):
    """Run one user script with its stdout captured (FastLED/fbuild#945).

    PlatformIO scripts routinely print progress banners; those must never
    reach the JSON protocol channel. Two capture layers: Python-level
    `print()` goes to a `redirect_stdout` buffer, and raw fd 1 writes
    (subprocesses, `os.write`) go to a temp file swapped in via `dup2`.
    Both are preserved in `notes` (truncated) and echoed to stderr for
    verbose logs.
    """
    buf = io.StringIO()
    saved_fd = os.dup(1)
    raw = tempfile.TemporaryFile(mode="w+", encoding="utf-8", errors="replace")
    os.dup2(raw.fileno(), 1)
    try:
        with contextlib.redirect_stdout(buf):
            run_script(env, path)
    finally:
        os.dup2(saved_fd, 1)
        os.close(saved_fd)
        raw.seek(0)
        text = buf.getvalue() + raw.read()
        raw.close()
        if text:
            sys.stderr.write(text)
            ledger.notes.append(
                f"script stdout ({os.path.basename(path)}): {text[-4096:]}"
            )


def main():
    if len(sys.argv) != 2:
        raise RuntimeFailure("usage: lite_scons_harness.py <input.json>")

    # Protocol guard (FastLED/fbuild#945): the JSON result is the ONLY
    # thing allowed on the real stdout. User scripts may print, and may
    # spawn subprocesses that inherit the raw stdout fd, so both layers
    # are sealed: fd 1 is repointed at stderr for the whole run, and the
    # duplicated original receives the final json.dump.
    sys.stdout.flush()
    protocol_fd = os.dup(sys.stdout.fileno())
    os.dup2(sys.stderr.fileno(), sys.stdout.fileno())

    with open(sys.argv[1], "r", encoding="utf-8") as fh:
        data = json.load(fh)

    project_dir = os.path.abspath(data["project_dir"])
    env_name = data["env_name"]
    project_options = data.get("project_options", {})
    board_config = data.get("board_config", {})
    platform_name = data.get("platform_name")
    platformio_home = data.get("platformio_home", os.path.expanduser("~/.platformio"))

    ledger = Ledger()
    ledger.snapshot_dir(project_dir)

    env = Environment(
        "env", project_dir, env_name, project_options,
        board_config, platform_name, platformio_home, ledger,
    )
    projenv = Environment(
        "projenv", project_dir, env_name, project_options,
        board_config, platform_name, platformio_home, ledger,
    )

    install_scons_module(env)

    unsupported = []
    try:
        script_entries = [
            resolve_script_entry(env, item) for item in data.get("extra_scripts", [])
        ]
        pre_scripts  = [path for scope, path in script_entries if scope == "pre"]
        post_scripts = [path for scope, path in script_entries if scope == "post"]
        for path in pre_scripts:
            run_script_captured(env, ledger, path)
        for path in post_scripts:
            run_script_captured(env, ledger, path)
    except RuntimeFailure as exc:
        unsupported.append(str(exc))
    except Exception as exc:
        unsupported.append(f"{type(exc).__name__}: {exc}")
        ledger.errors.append(traceback.format_exc())

    result = {
        "env":     env.export_state(),
        "projenv": projenv.export_state(),
        "notes":   ledger.notes,
        "unsupported": unsupported,
        "lite_scons_records": {
            "executed_actions":      ledger.executed_actions,
            "generated_files":       ledger.newly_generated(project_dir),
            "recorded_pre_actions":  ledger.recorded_pre_actions,
            "recorded_post_actions": ledger.recorded_post_actions,
            "custom_targets":        ledger.custom_targets,
            "middleware":            ledger.middleware,
            "builder_calls":         ledger.builder_calls,
            "errors":                ledger.errors,
        },
    }
    sys.stdout.flush()
    with os.fdopen(protocol_fd, "w", encoding="utf-8") as protocol_out:
        json.dump(result, protocol_out, default=str)


if __name__ == "__main__":
    main()
