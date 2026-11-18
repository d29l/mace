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
- An in-game bar, drawn inside the game, for your server, the server list
  and alts, on any desktop
- Accounts you aren't using run at a lower frame rate, and anti-AFK keeps
  idle ones from being kicked
- Mods for every account: cursor, shift lock cursor, death sound, font, and
  any other file, in the same layout as Bloxstrap's mods folder
- A loading window, and toasts in the game when you join, saying where the
  server is
- FastFlags, GameMode, MangoHud and per-account settings

## Requirements

- An x86-64 Linux system with a Vulkan driver
- [Rosemary](https://github.com/microlift/rosemary), installed through
  [Butter](https://github.com/microlift/butter)
- GTK 4 and libadwaita 1.5+ (and Rust, only to build Mace yourself)

On Arch:

```sh
sudo pacman -S gtk4 libadwaita
```

## Install

```sh
curl -L https://github.com/d29l/mace/releases/latest/download/mace-x86_64-linux.tar.gz | tar xz
cd mace-*/ && ./install.sh
```

This adds Mace to your app menu and makes the Play button on roblox.com open
it. If you used Nutmeg before, your settings and login are picked up
automatically.

To build it yourself instead:

```sh
git clone https://github.com/d29l/mace && cd mace
./install.sh
```

In a game, press Insert for the in-game bar (the key can be changed in Mace's
settings, and `mace panel` opens it too).

## Usage

Open Mace, add your accounts and sign in to each one. After that, use the
Play button on roblox.com, the Mace window, or the command line:

```sh
mace list                 # your accounts and what they're doing
mace start ACCOUNT        # start an account
mace gather ACCOUNT       # bring every other account to ACCOUNT's server
mace servers ACCOUNT      # the public servers of ACCOUNT's game
mace why ACCOUNT          # explain why a start failed
```

Run `mace --help` for everything else.

## Update

Mace checks for a new version once a day. When there is one, the Mace window
shows an Update button and your game shows a toast; `mace upgrade` installs it
from a terminal. Games that are running keep the old version until they're
restarted.

A copy installed with `./install.sh --dev` doesn't update itself: run
`git pull` and `./install.sh --dev` again.

## Uninstall

```sh
./uninstall.sh            # keeps your accounts
./uninstall.sh --purge    # removes everything
```

## License

Mace is free software under the [GNU General Public License v3.0 or later](LICENSE).
