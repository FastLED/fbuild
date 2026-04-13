import inspect
import json
import os
import re
import sys


SUPPORTED_SCOPES = {
    "CPPDEFINES",
    "CPPPATH",
    "CCFLAGS",
    "CFLAGS",
    "CXXFLAGS",
    "ASFLAGS",
    "LINKFLAGS",
    "LIBPATH",
    "LIBS",
}

NOOP_METHODS = {
    "AddPostAction",
    "AddPreAction",
    "AlwaysBuild",
    "Alias",
    "Depends",
}


def stringify_macro(value):
    return '\\"%s\\"' % str(value).replace('"', '\\\\\\"')


class RuntimeFailure(Exception):
    pass


class MockEnv:
    def __init__(self, label, project_dir, env_name, project_options, notes, unsupported):
        self._label = label
        self._project_dir = project_dir
        self._env_name = env_name
        self._project_options = project_options
        self._notes = notes
        self._unsupported = unsupported
        self._scopes = {key: [] for key in SUPPORTED_SCOPES}
        self._vars = {
            "PROJECT_DIR": project_dir,
            "PIOENV": env_name,
        }

    def export_state(self):
        return {
            "cppdefines": self._scopes["CPPDEFINES"],
            "cpppath": self._scopes["CPPPATH"],
            "ccflags": self._scopes["CCFLAGS"],
            "cflags": self._scopes["CFLAGS"],
            "cxxflags": self._scopes["CXXFLAGS"],
            "asflags": self._scopes["ASFLAGS"],
            "linkflags": self._scopes["LINKFLAGS"],
            "libpath": self._scopes["LIBPATH"],
            "libs": self._scopes["LIBS"],
        }

    def _record_unsupported(self, message):
        self._unsupported.append(message)
        raise RuntimeFailure(message)

    def _normalize_path(self, value):
        if value is None:
            return None
        resolved = self.subst(str(value))
        if os.path.isabs(resolved):
            return {"kind": "path", "value": resolved}
        return {"kind": "path", "value": os.path.join(self._project_dir, resolved)}

    def _normalize_cppdefine(self, value):
        if isinstance(value, (list, tuple)):
            if len(value) == 2 and not isinstance(value[0], (list, tuple, dict)):
                return [
                    {
                        "kind": "kv",
                        "key": str(value[0]),
                        "value": value[1],
                    }
                ]
            items = []
            for item in value:
                items.extend(self._normalize_cppdefine(item))
            return items
        if isinstance(value, dict):
            return [value]
        return [str(value)]

    def _normalize_generic(self, value):
        if isinstance(value, (list, tuple)):
            items = []
            for item in value:
                items.extend(self._normalize_generic(item))
            return items
        return [value]

    def _normalize_items(self, scope, value):
        if scope == "CPPDEFINES":
            return self._normalize_cppdefine(value)
        if scope in ("CPPPATH", "LIBPATH"):
            return [self._normalize_path(item) for item in self._normalize_generic(value)]
        return [str(item) for item in self._normalize_generic(value)]

    def _apply(self, scope, items, mode):
        current = self._scopes[scope]
        if mode == "replace":
            self._scopes[scope] = items
            return
        for item in items:
            if mode == "append":
                current.append(item)
            elif mode == "prepend":
                current.insert(0, item)
            elif mode == "append_unique":
                if item not in current:
                    current.append(item)

    def _mutate(self, mode, kwargs):
        for scope, value in kwargs.items():
            if scope not in SUPPORTED_SCOPES:
                self._record_unsupported(
                    f"{self._label}.{mode} on unsupported scope '{scope}'"
                )
            self._apply(scope, self._normalize_items(scope, value), mode)

    def Append(self, **kwargs):
        self._mutate("append", kwargs)

    def AppendUnique(self, **kwargs):
        self._mutate("append_unique", kwargs)

    def Prepend(self, **kwargs):
        self._mutate("prepend", kwargs)

    def Replace(self, **kwargs):
        self._mutate("replace", kwargs)

    def StringifyMacro(self, value):
        return stringify_macro(value)

    def subst(self, text):
        text = str(text)

        def repl(match):
            key = match.group(1) or match.group(2)
            return str(self._vars.get(key, self._project_options.get(key, match.group(0))))

        return re.sub(r"\$([A-Za-z_][A-Za-z0-9_]*)|\$\{([^}]+)\}", repl, text)

    def get(self, key, default=None):
        return self._vars.get(key, default)

    def GetProjectOption(self, key, default=None):
        return self._project_options.get(key, default)

    def __getitem__(self, key):
        if key in self._vars:
            return self._vars[key]
        if key in self._scopes:
            return self._scopes[key]
        raise KeyError(key)

    def __setitem__(self, key, value):
        if key in SUPPORTED_SCOPES:
            self._scopes[key] = self._normalize_items(key, value)
        else:
            self._vars[key] = value

    def __getattr__(self, name):
        if name in NOOP_METHODS:
            def _noop(*args, **kwargs):
                self._notes.append(f"{self._label}.{name} ignored by native extra_scripts runtime")
                return None

            return _noop
        self._record_unsupported(f"{self._label}.{name} is not supported")


def resolve_script_entry(env, raw):
    if ":" in raw:
        prefix, path = raw.split(":", 1)
        if prefix not in ("pre", "post"):
            raise RuntimeFailure(f"unsupported extra_scripts prefix '{prefix}'")
        return prefix, os.path.abspath(env.subst(path))
    return "post", os.path.abspath(env.subst(raw))


def execute_script(path, import_fn, project_dir):
    script_dir = os.path.dirname(path)
    old_sys_path = list(sys.path)
    sys.path.insert(0, script_dir)
    if project_dir not in sys.path:
        sys.path.insert(0, project_dir)
    scope = {
        "__file__": path,
        "__name__": "__main__",
        "Import": import_fn,
        "DefaultEnvironment": import_fn.default_environment,
        "COMMAND_LINE_TARGETS": [],
        "ARGUMENTS": {},
    }
    try:
        with open(path, "r", encoding="utf8") as handle:
            code = compile(handle.read(), path, "exec")
        exec(code, scope, scope)
    finally:
        sys.path[:] = old_sys_path


class ImportDispatcher:
    def __init__(self, env, projenv, allow_projenv):
        self._env = env
        self._projenv = projenv
        self._allow_projenv = allow_projenv

    def __call__(self, *names):
        frame = inspect.currentframe().f_back
        for name in names:
            if name == "env":
                frame.f_globals["env"] = self._env
            elif name == "projenv":
                if not self._allow_projenv:
                    raise RuntimeFailure("projenv is not available in PRE extra_scripts")
                frame.f_globals["projenv"] = self._projenv
            else:
                raise RuntimeFailure(f"Import('{name}') is not supported")

    def default_environment(self):
        return self._env


def main():
    if len(sys.argv) != 2:
        raise RuntimeFailure("usage: harness.py <input.json>")

    with open(sys.argv[1], "r", encoding="utf8") as handle:
        data = json.load(handle)

    project_dir = os.path.abspath(data["project_dir"])
    env_name = data["env_name"]
    project_options = data.get("project_options", {})
    notes = []
    unsupported = []
    env = MockEnv("env", project_dir, env_name, project_options, notes, unsupported)
    projenv = MockEnv("projenv", project_dir, env_name, project_options, notes, unsupported)

    script_entries = [resolve_script_entry(env, item) for item in data.get("extra_scripts", [])]
    pre_scripts = [path for scope, path in script_entries if scope == "pre"]
    post_scripts = [path for scope, path in script_entries if scope == "post"]

    try:
        for path in pre_scripts:
            execute_script(path, ImportDispatcher(env, projenv, False), project_dir)
        for path in post_scripts:
            execute_script(path, ImportDispatcher(env, projenv, True), project_dir)
    except RuntimeFailure as exc:
        unsupported.append(str(exc))

    result = {
        "env": env.export_state(),
        "projenv": projenv.export_state(),
        "notes": notes,
        "unsupported": unsupported,
    }
    json.dump(result, sys.stdout)


if __name__ == "__main__":
    main()
