# Troubleshooting

[← Back to the README](../README.md)

- [The Guide button does nothing](#the-guide-button-does-nothing)
- [Guide opens Steam instead of Orbit](#guide-opens-steam-instead-of-orbit)
- [The X button doesn't close programs (or Y does)](#the-x-button-doesnt-close-programs-or-y-does)
- [Orbit doesn't stay on top of the game](#orbit-doesnt-stay-on-top-of-the-game)
- [The Programs tab is empty](#the-programs-tab-is-empty)
- [An audio output is missing](#an-audio-output-is-missing)
- [Bluetooth won't turn on / "blocked by the system"](#bluetooth-wont-turn-on--blocked-by-the-system)
- [Bluetooth pairing fails](#bluetooth-pairing-fails)
- [Recording / Instant Replay don't work](#recording--instant-replay-dont-work)
- [Viewing the logs](#viewing-the-logs)

---

## The Guide button does nothing

1. **Is Orbit running?**
   ```sh
   pgrep -a orbit
   ```
   If nothing shows up, run `./install.sh` (or `orbit --daemon`).

2. **Was the gamepad detected?** Run it in the foreground and press Guide:
   ```sh
   orbit --quit; orbit --daemon
   ```
   You should see `orbit: controle conectado: <gamepad name>` ("gamepad connected").

3. **Device permission.** Your user must be able to read the gamepad:
   ```sh
   grep -A4 -i "controller\|gamepad\|joystick" /proc/bus/input/devices   # find the eventN
   getfacl /dev/input/eventN
   ```
   There should be a `user:<your user>:rw-` line (systemd's *uaccess* rule). If there
   isn't, add your user to the `input` group (`sudo usermod -aG input $USER`) and log in
   again.

4. **Does the gamepad send the Guide button?** Some gamepads have modes (Switch,
   D-input, X-input), and in some of them the Home button doesn't send `BTN_MODE`.
   Check with `evtest /dev/input/eventN` and switch the gamepad mode if needed.

## Guide opens Steam instead of Orbit

In *Steam → Settings → Controller*, turn off **"Guide button focuses Steam"**.

## The X button doesn't close programs (or Y does)

Orbit listens for the `BTN_X`/`BTN_NORTH` code (0x133), which is **X** on Xbox and
compatible gamepads using the `xpad` driver. Other drivers (e.g. PlayStation through
`hid-playstation`) number the buttons differently. You can always use the **✕** button
next to each item instead: move right with the D-pad and press **A**.

## Orbit doesn't stay on top of the game

The overlay uses *layer-shell*. Check that:

- Orbit was built with `gtk4-layer-shell` (`install.sh` warns you when it wasn't);
- your desktop supports layer-shell: COSMIC, KDE Plasma, Hyprland, Sway, niri… do;
  GNOME and X11 don't. There, Orbit opens as a regular fullscreen window.

When you run `orbit --daemon` in a terminal, the message
`layer-shell indisponível, usando janela comum em tela cheia` ("layer-shell unavailable,
using a regular fullscreen window") means it is in window mode.

## The Programs tab is empty

```sh
orbit --list-windows
```

- `backend: wayland (cosmic)`, `wayland` or `x11` with windows listed: it's working.
- `nenhum backend de janelas disponível` ("no window backend available"): the desktop
  doesn't expose its windows (GNOME on Wayland).

## An audio output is missing

```sh
orbit --list-audio
```

This shows exactly what the tab will list (`*` = default, `-` = disconnected). Outputs
with nothing plugged in are hidden under **"▾ Show disconnected outputs"**. If a port
doesn't show up even there, check `pactl list cards`: Orbit lists the ports that
PipeWire/PulseAudio report.

## Bluetooth won't turn on / "blocked by the system"

When the adapter is soft-blocked, the **Bluetooth** button runs
`rfkill unblock bluetooth` by itself. If it stays blocked:

```sh
rfkill list bluetooth
systemctl status bluetooth
```

- `Hard blocked: yes`: a physical switch or a BIOS option is turning Bluetooth off.
- service stopped: `sudo systemctl enable --now bluetooth`.

## Bluetooth pairing fails

- Put the device in **pairing mode** before pressing A (gamepads: usually hold the sync
  button; headsets: hold the power button).
- "didn't respond": the device left pairing mode or is too far away.
- If it was already paired with another OS on this PC (dual boot), forget it (**X**) and
  pair again.
- If a code appears, check that it matches the one on the device and choose **Confirm**.

## Recording / Instant Replay don't work

Install [`gpu-screen-recorder`](https://git.dec05eba.com/gpu-screen-recorder/about/).
On Wayland it asks for screen capture permission the first time.

## Viewing the logs

```sh
orbit --quit
orbit --daemon        # keeps the logs in the terminal
```

Log messages are currently in Portuguese.
