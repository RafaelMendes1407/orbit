# Architecture and development

[← Back to the README](../README.md)

## Overview

Orbit is a single Rust + GTK4 binary that stays **resident**: the window is built once
at startup and then only shown or hidden. Every external data source (gamepad, windows,
Bluetooth, audio) is read through events or off the UI thread, so the panel opens
instantly and the process uses no CPU while idle.

```text
                    ┌────────────────────────── UI thread (GTK) ───────────────────────┐
 /dev/input ──evdev──▶ gamepad.rs ─channel─▶ main.rs (Ui: tabs, focus, navigation)      │
 (one thread per pad)                          │                                        │
                                               ├─ games.rs     Steam + .desktop         │
 compositor ─Wayland/X11─▶ windows.rs ─channel▶├─ windows.rs   open programs            │
 (own thread)                                  ├─ audio.rs ────pactl (spawn_blocking)   │
 BlueZ ───────D-Bus (signals)────────────────▶ ├─ bluetooth.rs D-Bus + agent            │
                                               ├─ capture.rs   screenshots, gpu-screen-recorder
                                               └─ system.rs    shortcuts and power      │
                    └───────────────────────────────────────────────────────────────────┘
```

## Modules

| File | Responsibility |
|---|---|
| `src/main.rs` | GTK application, window (layer-shell or fullscreen), tabs, gamepad navigation, per-tab focus, command line |
| `src/gamepad.rs` | Finds gamepads in `/dev/input`, one blocking thread per gamepad, turns events into `PadEvent`, `EVIOCGRAB` while the panel is open |
| `src/games.rs` | Reads Steam's `appmanifest_*.acf` files and local cover art, plus `.desktop` files in the `Game` category |
| `src/windows.rs` | Window backends (COSMIC, wlr, KDE Plasma, X11/EWMH) and the Programs tab |
| `src/audio.rs` | Output/input model built from `pactl -f json` (cards, sinks, sources); switches profile, port and default |
| `src/bluetooth.rs` | BlueZ over D-Bus: adapter, devices, scanning, pairing and the `org.bluez.Agent1` agent |
| `src/capture.rs` | Screenshots (the desktop's native tool or the XDG portal), recording and Instant Replay |
| `src/system.rs` | Settings and Files (picked per desktop), lock and power |
| `src/util.rs` | Helpers: running processes, notifications, XDG folders, shared widgets, desktop detection |
| `src/style.css` | Theme |

## Gamepad and focus

- `gamepad.rs` treats a device as a gamepad when it has `BTN_SOUTH`/`BTN_MODE` and the
  `ABS_X` axis, and ignores Steam's virtual gamepad so events aren't duplicated.
- The stick uses hysteresis (engages at 60 %, releases below 35 %). Auto-repeat is done
  in the UI: 380 ms delay, then every 110 ms.
- Directional navigation happens **only inside the visible page**. This keeps focus from
  landing on the outgoing page during the tab-switch animation.
- Each tab keeps a weak reference to its last focused widget. If focus is lost (the
  widget was removed or disabled), it is restored on the next main-loop iteration.
- Lists that change on their own (Programs, Audio, Bluetooth) are only rebuilt when
  their content changes, and they put focus back on the same item, or on its neighbour
  if it went away.

## Open programs (`windows.rs`)

The backend is chosen at startup, based on what the display server offers:

| Order | Protocol | Desktops | Close | Activate |
|---|---|---|---|---|
| 1 | `ext_foreign_toplevel_list_v1` + `zcosmic_toplevel_info_v1` + `zcosmic_toplevel_manager_v1` | COSMIC | `manager.close` | `manager.activate` |
| 2 | `zwlr_foreign_toplevel_manager_v1` | Hyprland, Sway, niri, labwc, Wayfire, river | `handle.close` | `handle.activate(seat)` |
| 3 | `org_kde_plasma_window_management` | KDE Plasma | `window.close` | `set_state(active)` |
| 4 | `ext_foreign_toplevel_list_v1` alone | others | — (list only) | — |
| 5 | EWMH (`_NET_CLIENT_LIST`, `_NET_CLOSE_WINDOW`) | X11 sessions | `_NET_CLOSE_WINDOW` | `_NET_ACTIVE_WINDOW` |

The Wayland (or X11) connection is Orbit's own and runs on a thread that sleeps on the
socket. Wayland proxies are thread-safe, so the UI sends `close`/`activate` directly and
flushes. "Close" is the same request as the window's X button, so the program may ask
whether you want to save first. Orbit's own window is filtered out of the list.

## Audio (`audio.rs`)

The tab combines two sources:

1. **Existing nodes** (`pactl list sinks/sources`): picking one makes it the default with
   `set-default-*`.
2. **Card ports** that aren't active (`pactl list cards`): a port may need a different
   profile (e.g. `output:hdmi-stereo-extra1` for the second HDMI monitor). The profile is
   chosen by preferring: available, then the one that **keeps the other direction** of
   the current profile (so the microphone isn't dropped), then the highest priority.

When a port is picked: `set-card-profile` (if needed), wait for the node to appear (up
to 3 s), then `set-*-port` and `set-default-*`. Ports with `availability: not available`
only show up under "disconnected". Cards in the `pro-audio` profile are covered by their
nodes.

## Bluetooth (`bluetooth.rs`)

- Connects to the system bus through `gio`. State comes from
  `ObjectManager.GetManagedObjects`, and changes from the `InterfacesAdded`,
  `InterfacesRemoved` and `PropertiesChanged` signals, with rendering batched over ~80 ms.
- `StartDiscovery` runs only while the tab is open and the adapter is powered. Scanning
  stops before a `Pair` (some adapters fail to pair while scanning).
- What **A** does: not paired → `Pair` → `Trusted=true` → `Connect`; paired → `Connect`;
  connected → `Disconnect`.
- Orbit registers a `DisplayYesNo` agent at `/io/github/orbit/agent`. It is **not** the
  system's default agent, so BlueZ only uses it for pairings Orbit starts itself.
  `RequestConfirmation` shows the code with Confirm/Cancel; `DisplayPasskey` and
  `DisplayPinCode` show the code to type.
- Turning Bluetooth on while the adapter is soft-blocked tries `rfkill unblock bluetooth`.

## Building

```sh
cargo build --release                        # with layer-shell (default)
cargo build --release --no-default-features  # without layer-shell (window mode)
```

| Feature | Default | Effect |
|---|---|---|
| `layer-shell` | yes | Real overlay through `gtk4-layer-shell` (needs the system library) |

## Development tools

```sh
orbit --list-windows                  # window backend and what it sees
orbit --list-audio                    # Audio tab model, with each item's action
orbit --render-docs docs/screenshots  # regenerate the documentation screenshots
```

`--render-docs` starts a separate instance (it doesn't talk to the resident one), opens
the panel in window mode, waits for the data to load and renders each tab's panel to a
PNG with `gtk::WidgetPaintable`. It only captures the panel, never the rest of the screen.

## Ideas for the future

- Force-quit a frozen program (e.g. hold X).
- Configurable button mapping (PlayStation/Nintendo gamepads).
- Battery level of Bluetooth devices (`org.bluez.Battery1`).
- English translation of the interface.
