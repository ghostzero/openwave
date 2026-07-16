#!/usr/bin/env python3
"""OpenWave VST host helper.

Hosts one channel's VST plugins headlessly through Carla's engine library
(no Carla UI), controlled by OpenWave over a JSON-lines protocol:

  stdin  <- {"cmd": "load", "plugins": [{"cfg_id", "path", "format",
             "label", "active", "params": {"<index>": value}}, ...]}
  stdin  <- {"cmd": "set", "cfg_id": N, "param": I, "value": V}
  stdin  <- {"cmd": "active", "cfg_id": N, "on": true}
  stdin  <- {"cmd": "quit"}
  fd 3?  -> no; replies go to the original stdout (dup'ed before Carla can
            write its own logs there):
            {"reply": "ready"}
            {"reply": "loaded", "plugins": [{"cfg_id", "ok", "name",
              "error", "params": [{"index", "name", "min", "max", "def",
              "value", "toggled", "integer"}]}]}

The engine registers as a JACK client (PipeWire) named after argv[1], in
continuous-rack mode: fixed stereo in/out ports that OpenWave wires with
pw-link. argv[2] is the Carla resource prefix (lib dir).
"""

import json
import os
import select
import sys
import time

CLIENT_NAME = sys.argv[1] if len(sys.argv) > 1 else "OpenWave_VST"

# Protocol writes must not interleave with Carla's own stdout logging:
# keep a private dup of stdout and point fd 1 at stderr before the engine
# library gets a chance to print anything.
PROTO = os.fdopen(os.dup(1), "w", buffering=1)
os.dup2(2, 1)


def send(obj):
    PROTO.write(json.dumps(obj) + "\n")
    PROTO.flush()


def find_first(paths):
    for p in paths:
        if os.path.exists(p):
            return p
    return None


CARLA_SHARE = find_first([
    "/usr/share/carla",
    "/usr/local/share/carla",
])
CARLA_LIB = find_first([
    "/usr/lib64/carla/libcarla_standalone2.so",
    "/usr/lib/carla/libcarla_standalone2.so",
    "/usr/lib/x86_64-linux-gnu/carla/libcarla_standalone2.so",
    "/usr/local/lib/carla/libcarla_standalone2.so",
])
if not CARLA_SHARE or not CARLA_LIB:
    send({"reply": "fatal", "error": "Carla is not installed"})
    sys.exit(1)

sys.path.insert(0, CARLA_SHARE)
from carla_backend import (  # noqa: E402
    CarlaHostDLL,
    BINARY_NATIVE,
    ENGINE_OPTION_OSC_ENABLED,
    ENGINE_OPTION_PATH_BINARIES,
    ENGINE_OPTION_PROCESS_MODE,
    ENGINE_PROCESS_MODE_CONTINUOUS_RACK,
    PARAMETER_INPUT,
    PARAMETER_IS_BOOLEAN,
    PARAMETER_IS_ENABLED,
    PARAMETER_IS_INTEGER,
    PLUGIN_VST2,
    PLUGIN_VST3,
)

host = CarlaHostDLL(CARLA_LIB, False)
host.set_engine_option(ENGINE_OPTION_PROCESS_MODE, ENGINE_PROCESS_MODE_CONTINUOUS_RACK, "")
host.set_engine_option(ENGINE_OPTION_PATH_BINARIES, 0, os.path.dirname(CARLA_LIB))
# One engine runs per channel; without this they all race for the same
# OSC control ports and the losers fail.
host.set_engine_option(ENGINE_OPTION_OSC_ENABLED, 0, "")


def engine_callback(handle, action, plugin_id, value1, value2, value3, valuef, value_str):
    pass


def file_callback(ptr, action, is_dir, title, filter_str):
    return ""


host.set_engine_callback(engine_callback)
host.set_file_callback(file_callback)

if not host.engine_init("JACK", CLIENT_NAME):
    send({"reply": "fatal", "error": host.get_last_error() or "engine init failed"})
    sys.exit(1)

send({"reply": "ready"})

# cfg_id (OpenWave's stable id) -> Carla plugin index. Indices never shift
# because the plugin set is fixed per process: structural changes respawn
# the helper.
plugin_ids = {}


def param_dump(pid):
    out = []
    for i in range(host.get_parameter_count(pid)):
        data = host.get_parameter_data(pid, i)
        if data["type"] != PARAMETER_INPUT or not (data["hints"] & PARAMETER_IS_ENABLED):
            continue
        info = host.get_parameter_info(pid, i)
        rng = host.get_parameter_ranges(pid, i)
        out.append({
            "index": i,
            "name": info["name"] or f"Parameter {i}",
            "min": rng["min"],
            "max": rng["max"],
            "def": rng["def"],
            "value": host.get_current_parameter_value(pid, i),
            "toggled": bool(data["hints"] & PARAMETER_IS_BOOLEAN),
            "integer": bool(data["hints"] & PARAMETER_IS_INTEGER),
        })
    return out


def cmd_load(msg):
    results = []
    for p in msg.get("plugins", []):
        cfg_id = p["cfg_id"]
        ptype = PLUGIN_VST3 if p.get("format") == "vst3" else PLUGIN_VST2
        label = p.get("label") or None
        try:
            ok = host.add_plugin(BINARY_NATIVE, ptype, p["path"], None, label, 0, None, 0)
        except Exception as e:  # defensive: a bad binary must not kill us
            ok = False
        if not ok:
            results.append({
                "cfg_id": cfg_id,
                "ok": False,
                "error": host.get_last_error() or "could not load plugin",
            })
            continue
        pid = host.get_current_plugin_count() - 1
        plugin_ids[cfg_id] = pid
        for key, value in (p.get("params") or {}).items():
            try:
                host.set_parameter_value(pid, int(key), float(value))
            except Exception:
                pass
        # Backend-added plugins do not start processing on their own.
        host.set_active(pid, bool(p.get("active", True)))
        info = host.get_plugin_info(pid)
        results.append({
            "cfg_id": cfg_id,
            "ok": True,
            "name": info["name"] or os.path.basename(p["path"]),
            "params": param_dump(pid),
        })
    send({"reply": "loaded", "plugins": results})


def handle(msg):
    cmd = msg.get("cmd")
    if cmd == "load":
        cmd_load(msg)
    elif cmd == "set":
        pid = plugin_ids.get(msg.get("cfg_id"))
        if pid is not None:
            host.set_parameter_value(pid, int(msg["param"]), float(msg["value"]))
    elif cmd == "active":
        pid = plugin_ids.get(msg.get("cfg_id"))
        if pid is not None:
            host.set_active(pid, bool(msg["on"]))
    elif cmd == "quit":
        raise SystemExit(0)


buf = b""
try:
    while True:
        host.engine_idle()
        ready, _, _ = select.select([0], [], [], 0.03)
        if not ready:
            continue
        chunk = os.read(0, 65536)
        if not chunk:
            break  # OpenWave went away
        buf += chunk
        while b"\n" in buf:
            line, buf = buf.split(b"\n", 1)
            if not line.strip():
                continue
            try:
                handle(json.loads(line))
            except SystemExit:
                raise
            except Exception as e:
                send({"reply": "error", "error": str(e)})
finally:
    try:
        host.engine_close()
    except Exception:
        pass
