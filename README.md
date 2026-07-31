# BNK Sound

[![CI](https://github.com/borgenk/bnksound/actions/workflows/ci.yml/badge.svg)](https://github.com/borgenk/bnksound/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/borgenk/bnksound)](https://github.com/borgenk/bnksound/releases/latest)

A native audio mixer for Wayland, with per-app volume and savable profiles.

_Disclaimer: personal project, built mainly for my own use, partly AI-assisted._

![BNK Sound](assets/screenshot.png)

## Requirements

- A Linux desktop running a **Wayland** session
- **PipeWire** 0.3+ running as the audio server
- **GTK4** runtime libraries (optional)

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/borgenk/bnksound/main/install.sh | sh
```

This drops the binary in `~/.local/bin` and installs the desktop entry and icon.

### Undecorated build

Without window decorations or GTK, for tiling compositors and similar:

```sh
curl -fsSL https://raw.githubusercontent.com/borgenk/bnksound/main/install.sh | sh -s -- --undecorated
```

### Flatpak

Each release carries a bundle of the GTK build. Download it from the
[releases page](https://github.com/borgenk/bnksound/releases/latest) and:

```sh
flatpak install --user ./bnksound-*.flatpak
```

The GNOME runtime it needs comes from Flathub, so that remote has to be
configured.

## Configuration

State lives under the XDG config dir (`$XDG_CONFIG_HOME/bnksound`, falling back
to `~/.config/bnksound`):

- `state.bin` - saved profiles and the active selection
- `settings.conf` - visual toggles
