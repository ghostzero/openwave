# OpenWave

A dual-mix virtual audio mixer for Linux, built with GTK4/libadwaita on top of
PipeWire. OpenWave routes your microphone, applications, and virtual devices
into two independent mixes — inspired by Elgato Wave Link, but without
requiring any specific hardware.

![OpenWave](img.png)

## The dual-mix concept

Every input channel has **two independent faders**:

- **Monitor Mix** — what *you* hear. Routed to a hardware output of your
  choice (headphones, speakers).
- **Stream Mix** — what *your audience* hears. Exposed as a virtual
  microphone called **“Virtual Stream Mix”** that you select as the input
  device in OBS, Discord, Zoom, or any other application.

This lets you, for example, listen to music loudly while streaming it
quietly, or hear a voice chat that never reaches your stream at all.

## Features

- **Dynamic input channels** (up to 8): starts with *Microphone* and
  *System*; add more (Game, Music, Voice Chat, Browser, SFX, Aux, or custom
  names) with the **+** card, remove them again anytime.
- **Three kinds of channel inputs:**
  - *Capture sources* — microphones, line-ins, or monitors of other devices.
  - *Applications* — running playback streams, matched by application name
    and moved into the channel automatically.
  - *Virtual devices* — the channel appears as a selectable output device
    named `OpenWave: <channel>`. Point Discord's output at
    `OpenWave: Voice Chat`, or set `OpenWave: System` as your system default
    output; OBS can also capture these devices directly ("Audio Output
    Capture (PulseAudio)").
- **Per-channel effects**: insert a chain of **LV2 plugins** (noise gates,
  compressors, EQs, …) on any input, edited directly in OpenWave with live
  parameter control — plus an optional **Carla rack for VST2/VST3 plugins**.
  Effects are applied before the monitor/stream split, so both mixes hear
  the processed signal.
- **Per-channel, per-mix volume and mute**, with optional fader linking.
- **Master volume and mute** for both mixes, plus live level meters
  everywhere.
- **Self-healing routing**: OpenWave re-applies volumes and re-attaches
  streams if the session manager moves them, and cleans up stale devices
  from crashed sessions on startup.
- **Background operation**: closing the window keeps the virtual devices
  running; enable *Start at Login* in the main menu and OpenWave launches
  hidden on login, so your audio setup is always ready.
- Configuration persists across restarts at
  `~/.config/openwave/config.json`.

## Requirements

- Linux with **PipeWire** and its PulseAudio compatibility layer
  (`pipewire-pulse`) — the default on Fedora, Ubuntu 22.10+, and most
  current distributions.
- **GTK 4.18+** and **libadwaita 1.8+**.
- WirePlumber (or another PipeWire session manager).

Optional, for effects:

- **LV2 chains** need PipeWire's filter-chain LV2 support and the lilv
  library — on Fedora: `sudo dnf install pipewire-module-filter-chain-lv2
  lilv` — plus some LV2 plugins (`lsp-plugins-lv2` is a great start; the
  RNNoise-based `noise-suppression-for-voice` is popular for microphones).
  On Debian/Ubuntu the LV2 loader ships with PipeWire itself; install
  `liblilv-0-0` and e.g. `lsp-plugins-lv2`.
- **VST racks** need **Carla** (`sudo dnf install Carla` / `sudo apt
  install carla`). Windows VSTs work through yabridge as usual, since the
  rack is a regular Carla project.

Build dependencies (Fedora):

```sh
sudo dnf install gtk4-devel libadwaita-devel pulseaudio-libs-devel
```

Build dependencies (Debian/Ubuntu):

```sh
sudo apt install libgtk-4-dev libadwaita-1-dev libpulse-dev
```

## Building and installing

```sh
make            # cargo build --release
make install    # installs to ~/.local by default
```

`make install` places the binary, the desktop entry, and the app icon under
`$(PREFIX)` (default `~/.local`); pass `PREFIX=/usr/local` for a system-wide
install. Make sure `~/.local/bin` is on your `PATH`, then launch **OpenWave**
from your app grid, or run `openwave` directly. `make uninstall` removes
everything again.

## Quick start

1. Start OpenWave. It creates the virtual devices automatically.
2. Assign your microphone to the *Microphone* channel.
3. Set `OpenWave: System` as your default output in system sound settings so
   desktop audio flows through the *System* channel.
4. In OBS/Discord, select **“Virtual Stream Mix”** as the microphone.
5. Pick your headphones as the *Monitor Mix* output device in the Outputs
   section — and mix away.

### Effects

Click the puzzle-piece button on a channel strip to open its effects. *Add
Effect…* lists your installed LV2 plugins; each effect can be reordered,
bypassed, and tweaked with live parameter sliders. Enabling the **VST Rack**
inserts a Carla instance in front of the LV2 chain — *Open* brings up
Carla's window, where you add and configure VST plugins; save the rack
there (Ctrl+S) and OpenWave restores it (headless) on the next start.
Closing Carla's window simply bypasses the rack until you open it again.

## How it works

OpenWave talks to PipeWire through the PulseAudio client API on the GTK main
loop. It creates two null-sink buses (`OpenWave_Monitor`, `OpenWave_Stream`),
routes every channel through a pair of loopback streams (one per mix, each
carrying its own volume and mute), exposes the stream bus as a real capture
device via a remap source, and drives the level meters with low-rate
peak-detect streams. All streams carry unique names and opt out of
session-manager volume/target restoring, so the routing stays exactly as
configured.

Effect chains run out-of-process: each channel with effects gets a small
`pipewire -c` child hosting a `filter-chain` module (sink in, source out),
generated from your chain at `~/.config/openwave/fx/`. Parameter changes are
applied live via `pw-cli set-param`; a crashing plugin can't take OpenWave
down, and the channel falls back to its direct wiring. The Carla rack is a
separate child process wired in with `pw-link`; its project lives at
`~/.config/openwave/carla/ch<id>.carxp`.

## License

MIT
