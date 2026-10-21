"""Loads the group end-to-end encryption library (crates/pigeonpost-groupcrypto, UniFFI Python).

The packages carry the generated binding and its native library for each processor:
`pigeonpost/native/<x86_64|aarch64>/{pigeonpost_groupcrypto.py, libpigeonpost_groupcrypto_ffi.so}`.
When neither is present, or it does not load, groups stay hidden: there is no weaker fallback
(docs/groups.md §0).
"""

import importlib.util
import os
import platform
import threading
from pathlib import Path

ENC_SCHEME = "mls-rfc9420-x25519-aes128gcm-ed25519+ppgc1"
MODULE = "pigeonpost_groupcrypto"
LIBRARY = "libpigeonpost_groupcrypto_ffi.so"
ARCHES = {"x86_64": "x86_64", "amd64": "x86_64", "aarch64": "aarch64", "arm64": "aarch64"}

_lock = threading.Lock()
_loaded = None
_reason = None


def arch():
    return ARCHES.get(platform.machine().lower(), platform.machine().lower())


def candidates():
    """Directories that may hold the binding and its library, most specific first."""
    override = os.environ.get("PIGEONPOST_GROUPCRYPTO_DIR")
    if override:
        yield Path(override)
    native = Path(__file__).resolve().parent / "native"
    yield native / arch()


def load():
    """The binding module, or None (see `reason()`). Loaded once per process."""
    global _loaded, _reason
    with _lock:
        if _loaded is not None or _reason is not None:
            return _loaded
        for directory in candidates():
            source = directory / (MODULE + ".py")
            if not source.is_file() or not any(directory.glob("libpigeonpost_groupcrypto_ffi.*")):
                continue
            try:
                spec = importlib.util.spec_from_file_location("pigeonpost._groupcrypto_binding", source)
                module = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(module)
                if module.enc_scheme() != ENC_SCHEME:
                    raise RuntimeError("unexpected protocol " + module.enc_scheme())
            except Exception as exc:  # a broken or foreign library hides groups, nothing else
                _reason = f"{source.parent}: {type(exc).__name__}: {exc}"
                return None
            _loaded = module
            return module
        _reason = "the group encryption library is not installed for " + arch()
        return None


def reason():
    load()
    return _reason
