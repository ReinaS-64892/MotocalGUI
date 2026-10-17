<p align="center">
  <img src="data/dev.oblivius.spacecal-for-monado.svg" width="128" height="128" alt="SpaceCal for Monado">
</p>

<h1 align="center">SpaceCal for Monado</h1>

<p align="center">
  Align mixed VR tracking spaces on Linux
</p>

---

<p align="center">
  <img src="docs/screenshot-app.png" alt="SpaceCal main window" width="49%">
  <img src="docs/screenshot-instructions.png" alt="Calibration instructions" width="49%">
</p>

SpaceCal uses [libmotoc](https://docs.rs/libmotoc/0.4.0/libmotoc/) for sampled calibration, tracking-origin corrections, recentering, and OpenXR initialization. SpaceCal aligns VR devices from different tracking systems into a single unified space through the Monado OpenXR runtime. Use your Quest headset with lighthouse-tracked controllers, Vive trackers, or any mix of tracking technologies.

## Features

- **Calibrate** — grip both devices together, sweep a figure-eight, done. Audio cues and a progress bar guide you through.
- **Recenter** — face forward and press Recenter. Floor height is preserved.
- **Reset** — reset the target tracking origin, any tracking origin from the dropdown, or the entire STAGE/LOCAL reference space (position, orientation, and height).
- **Device identification** — open a dropdown and move a device to see which one it is.
- **Battery status** — see charge levels for all your tracked devices.
- **Shared calibration** — libmotoc reads and writes motoc’s default `last` profile at `$XDG_CONFIG_HOME/motoc/last.json` (usually `~/.config/motoc/last.json`). The UI restores it on startup and after reconnecting to the runtime; `motoc continue` loads the same file. Continuous profiles created by the CLI resume through libmotoc until a reset, a new calibration, or closing the UI.

## Installation

### Arch Linux (AUR)

```bash
yay -S spacecal-for-monado
```

### Debian / Ubuntu

Download the `.deb` from the [latest release](https://github.com/99oblivius/spacecal-for-monado/releases/latest):

```bash
sudo dpkg -i spacecal-for-monado_*.deb
sudo apt-get install -f
```

### Fedora

Download the `.rpm` from the [latest release](https://github.com/99oblivius/spacecal-for-monado/releases/latest):

```bash
sudo dnf install spacecal-for-monado-*.rpm
```

### NixOS

```bash
nix run github:99oblivius/spacecal-for-monado
```

### From source

```bash
# Dependencies (Arch)
sudo pacman -S gtk4 libadwaita openxr monado cargo

# Dependencies (Fedora)
sudo dnf install gtk4-devel libadwaita-devel openxr-devel monado-devel cargo

# Build and install
git clone https://github.com/99oblivius/spacecal-for-monado.git
cd spacecal-for-monado
cargo build --release --locked
sudo make PREFIX=/usr install
```

## Usage

Requires a Monado/WiVRn runtime exposing libmonado API 1.6 or later, `XR_MND_headless`, and `XR_MNDX_xdev_space`. Sample progress counts tracked poses; calibration times out after 120 seconds if tracking does not recover. Calibration and recentering offset writes are performed by libmotoc. Explicit resets and restoring saved tracking-origin offsets use libmonado directly, following the motoc CLI implementation. Other direct libmonado access reads device and battery information. Calibration and recentering follow libmotoc’s behavior. Device-based floor calibration and confidence scoring are not provided.

Calibration files are owned entirely by libmotoc, with the same format and profile name as the CLI. Existing profiles are read without being rewritten; a successful new calibration replaces `last.json`. A missing profile is normal on first use; unreadable or invalid profiles are reported without overwriting them. UI-only preferences (device selections, help visibility, and sample count) remain separate at `$XDG_DATA_HOME/spacecal-for-monado/config.json` (usually `~/.local/share/spacecal-for-monado/config.json`). The earlier UI-specific `motoc/spacecal-for-monado.json` profile is not imported automatically.

1. Start your Monado-based runtime (WiVRn, Monado standalone, etc.)
2. Launch SpaceCal
3. Select your **source** (reference device) and **target** (device to align)
4. Grip both devices firmly together and click **Calibrate**
5. Sweep a wide figure-eight through the air until the chime

## Acknowledgments

- [OpenVR-SpaceCalibrator](https://github.com/pushrax/OpenVR-SpaceCalibrator) by pushrax — original calibration framework concept
- [motoc](https://github.com/galister/motoc) by galister — calibration and runtime foundation (libmotoc)
- Proof of concept was developed with Claude Opus 4.7

## License

GPL-3.0-only
