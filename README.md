<p align="center">
  <img src="data/mace.svg" alt="Mace logo" width="140">
</p>

<h1 align="center">Mace</h1>

<p align="center">
  A Roblox launcher for Linux, with multiple accounts built in.
</p>

<p align="center">
  <a href="https://discord.gg/xkKTJrUQqs">
    <img src="https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white" alt="Discord">
  </a>
</p>

Mace runs the macOS Roblox client on Linux through
[Rosemary](https://github.com/microlift/rosemary). Every account gets its own
prefix, so you can play on several of them at the same time.

## Features

- Several accounts running side by side, each with its own login
- Each account gets the Roblox version it needs, so the updater doesn't get in the way
- Join another account's server, or bring every account to yours
- An in-game panel for your server, FPS and alts
- A loading window, and join notifications that show where the server is
- FastFlags, GameMode, MangoHud and per-account settings

## Requirements

- An x86-64 Linux system with a Vulkan driver
- [Rosemary](https://github.com/microlift/rosemary), installed through
  [Butter](https://github.com/microlift/butter)
- GTK 4 and libadwaita 1.5+, and Rust to build Mace
- Optional: [Quickshell](https://quickshell.org) for the in-game panel

On Arch:

```sh
sudo pacman -S rust gtk4 libadwaita
```

## Install

```sh
git clone https://github.com/d29l/mace && cd mace
./install.sh --toasts --overlay
```

This builds Mace, adds it to your app menu and makes the Play button on
roblox.com open it. If you used Nutmeg before, your settings and login are
picked up automatically.

Leave out `--toasts` or `--overlay` if you don't want join notifications or the
in-game panel. On Hyprland the panel opens with Super+Tab; elsewhere, bind a key
to `qs -p ~/.local/share/mace/app/overlay ipc call overlay toggle`.

## Usage

Open Mace, add your accounts and sign in to each one. After that, use the
Play button on roblox.com, the Mace window, or the command line:

```sh
mace list                 # your accounts and what they're doing
mace start ACCOUNT        # start an account
mace gather ACCOUNT       # bring every other account to ACCOUNT's server
mace why ACCOUNT          # explain why a start failed
```

Run `mace --help` for everything else.

## Uninstall

```sh
./uninstall.sh            # keeps your accounts
./uninstall.sh --purge    # removes everything
```

## License

Mace is free software under the [GNU General Public License v3.0 or later](LICENSE).
