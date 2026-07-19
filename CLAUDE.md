# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

OpenWave is a dual-mix virtual audio mixer for Linux (GTK4/libadwaita + PipeWire), written in Rust. Every input channel has two independent faders: a Monitor Mix (what the user hears) and a Stream Mix (exposed as a virtual microphone "Virtual Stream Mix" for OBS/Discord).

## Commands

```sh
make            # cargo build --release
make run        # cargo run (debug)
make check      # cargo clippy -- -D warnings  +  desktop-file-validate
make install    # installs binary/desktop file/icon to ~/.local (PREFIX=… to override)
```

There are no tests; `make check` (clippy with warnings denied) is the gate. Runtime needs PipeWire with `pipewire-pulse`; build needs `gtk4-devel libadwaita-devel pulseaudio-libs-devel` (Fedora names).

Diagnostic flags on the built binary: `openwave --list-lv2`, `openwave --list-vst`, and `--hidden` (autostart mode: window starts hidden).

## Architecture

Single-threaded: everything — PulseAudio client API, child-process I/O, UI — runs on the GTK main loop (`libpulse-glib-binding` mainloop adapter). The only worker threads are transient startup warm-up scans (LV2 catalog via `lv2::warm`, VST cache); everything else is async via glib timeouts/idles and PulseAudio operation callbacks. Never call `lv2::catalog()` on a path that runs while the UI should stay responsive unless the warm-up has landed (`lv2::scan_pending()` / `lv2::when_ready`).

### Modules

- `src/audio.rs` — `PulseManager`, the routing core. Talks to PipeWire via the PulseAudio API. Creates two null-sink buses (`OpenWave_Monitor`, `OpenWave_Stream`), routes each channel through a pair of `module-loopback`s (one per mix — per-channel/per-mix volume+mute are plain sink-input operations on those loopbacks), exposes the stream bus as a capture device via `module-remap-source`, and drives level meters with low-rate peek streams. Emits `AudioEvent`s consumed by the UI. Self-healing: re-applies volumes and re-attaches streams (both the sink and the capture side) if the session manager moves them; channels whose capture device is absent are parked and wired when it appears; cleans up stale `OpenWave_*` modules from crashed sessions on startup. Capture-before-playback ordering: the monitor-out loopback is deferred at startup (and bounced when a parked capture device on the same card appears) until capture loopbacks run, because the Wave XLR's firmware delivers a silent mic if its playback stream opens first; an 8s safety timeout guarantees monitor audio regardless.
- `src/config.rs` — serde config, persisted at `~/.config/openwave/config.json`. `Assignment` enum defines the three channel input kinds: `Source` (capture device), `App` (playback stream matched by `application.name`, moved into a per-channel null sink), `Virtual` (channel is itself a selectable output device "OpenWave: <name>").
- `src/fx.rs` — `FxManager`, per-channel effect chains as child processes (tied to app lifetime via `PR_SET_PDEATHSIG`). Two helper kinds per channel: an LV2 chain (`pipewire -c <generated conf>` hosting `module-filter-chain`; conf generated under `~/.config/openwave/fx/`; live control changes via `pw-cli set-param`) and a VST rack (`data/vsthost.py` embedded via `include_str!`, driving Carla's engine library headlessly over a JSON-lines stdin/stdout pipe). The VST helper is a JACK client, invisible to the PulseAudio API, so it's wired with `pw-link` against a dedicated null sink's monitor. VST rack processes first, then the LV2 chain. Structural VST changes (add/remove/reorder/enable) respawn the helper; parameter changes stream live. A crashed helper falls back to the channel's direct wiring.
- `src/lv2.rs` — LV2 discovery through liblilv, dlopen'd at runtime (`libloading`) so lilv is optional: without it the plugin browser is unavailable but existing chains still work (PipeWire instantiates them itself). Only 1-in/1-out and 2-in/2-out plugins are listed.
- `src/vst.rs` — VST2/VST3 discovery via `carla-discovery-native` (probes each binary in a throwaway process); results cached in `~/.cache/openwave/vst-scan.json` keyed by path + mtime.
- `src/ui/` — `window.rs` holds the `App` struct wiring everything together; `channel_strip.rs`, `outputs.rs`, `sidebar.rs`, `effects.rs` (the effects dialog, including live sync with a VST plugin's native editor window via `DialogHooks`), `setup.rs` (audio setup assistant: checks default output/input and Wave XLR routing, one-click fixes; auto-shown on first run, later misconfigurations raise a toast), `wave_xlr.rs` (Wave XLR volume dialog; stored levels are enforced for the first ~15s after start/device appearance — a one-shot write loses races against WirePlumber's route restore and the firmware's own resets — then the physical controls are left alone). Shared CSS lives in `ui/mod.rs`.
- `build.rs` compiles `data/openwave.gresource.xml` (bundled symbolic icons) into the binary.

### Conventions and gotchas

- The UI follows the [GNOME Human Interface Guidelines](https://developer.gnome.org/hig/) and is built with GTK4 + libadwaita: prefer adwaita widgets (`adw::` over raw `gtk::` where an equivalent exists), symbolic icons, and HIG-conformant spacing, capitalization (header capitalization for buttons/titles), and dialog patterns.
- Everything created on the audio server is prefixed `OpenWave_` (e.g. `OpenWave_Ch<id>`, `OpenWave_Ch<id>_FX`); startup cleanup and self-healing depend on this prefix.
- Every loopback/meter stream gets a unique, stable `media.name` and **opts out of session-manager volume/target restoring** (see `loopback_args` in `audio.rs`) — without this WirePlumber "restores" streams onto whatever a same-named stream used in an earlier session. Preserve these properties on any new stream/module.
- Module arguments are string-built; descriptions pass through `sanitize_desc` to avoid breaking PulseAudio quoting.
- Config saves are debounced through `schedule_save` (`ui/window.rs`), but quit and window-close save immediately — closing the window only hides it (virtual devices keep running in the background); `app.quit` saves, unloads everything from the audio server, then exits.
- Effect chain node labels use the effect's stable `id` (unique per channel) so reordering doesn't break the generated filter-chain graph.
