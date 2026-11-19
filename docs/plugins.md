# Mace plugins

A plugin adds a button and a menu to Mace's in-game bar, and can act on what
happens in your game: joining a server, leaving, opening the bar. Plugins are
written in [Luau](https://luau.org), the language Roblox scripts use, so if
you've scripted in Studio you already know most of it.

Plugins run in Mace, never inside Roblox. Each one gets its own sandbox: no
files, no shell and no network, only what the `mace` table below offers.

- [Your first plugin](#your-first-plugin)
- [How a plugin is laid out](#how-a-plugin-is-laid-out)
- [Permissions](#permissions)
- [The `mace` table](#the-mace-table)
- [Events](#events)
- [Menus: `mace.ui`](#menus-maceui)
- [Icons](#icons)
- [Storage](#storage)
- [Limits and errors](#limits-and-errors)

## Your first plugin

```sh
mace plugins new hello
mace plugins on hello
```

This makes `~/.local/share/mace/plugins/hello/`, with a working plugin in it:
a star in the bar whose menu counts your clicks. (Mace Settings → Plugins →
New does the same.) Start a game, press Insert, and there it is.

Open `init.luau` in any editor. Save it, and the plugin loads again in your
game within a second: no restart.

## How a plugin is laid out

```
plugins/hello/
├── plugin.json   what it is
├── init.luau     the script that runs
└── icon.png      anything else it uses (optional)
```

`plugin.json`:

```json
{
  "id": "hello",
  "name": "Hello",
  "version": "1.0.0",
  "api": 1,
  "author": "you",
  "description": "What it does, in a few words",
  "main": "init.luau",
  "permissions": []
}
```

- `id` is the folder's name: lowercase letters, digits and `-`.
- `api` is the plugin API it was written for. This document is API 1. A
  plugin made for another API isn't loaded, and Mace Settings says why.
- `main` is the script Mace runs (`init.luau` if left out).
- `permissions`: see below.

Plugins are off until you turn them on, in Mace Settings → Plugins or with
`mace plugins on ID`. Mace asks first, saying what the plugin may do.

A plugin runs once for each game window: with two accounts playing, each has
its own copy of it, and `mace.me` says which account it's with.

Other `.luau` files in the folder can be loaded with `require("util")` (or
`require("./util")`), which returns what the file returns.

## Permissions

What any plugin can do: show a button and a menu, show notes (toasts), keep
its own storage, use timers, and see the game and server its account is in.

Anything more needs a permission in `plugin.json`:

| Permission  | Allows                                                   |
|-------------|----------------------------------------------------------|
| `join`      | `mace.join`, `mace.rejoin`, `mace.hop`                   |
| `accounts`  | `mace.accounts`, `mace.start`, `mace.stop`, `mace.gather`|
| `clipboard` | `mace.copy`                                              |

Calling one of these without its permission is an error that names the
permission it needs.

## The `mace` table

### About the plugin and its account

| Name          | What it is                                                      |
|---------------|-----------------------------------------------------------------|
| `mace.api`    | `1`                                                             |
| `mace.plugin` | `{ id, name, version }`                                         |
| `mace.me`     | the account: `{ id, name, username, user }` (`user`: Roblox user id, or nil) |

### `mace.game()`

The server this account is in, or `nil` when it isn't in one:

```lua
{
    place = "1818",          -- place id (text)
    job = "9d99…",           -- the server's job id
    universe = "232",        -- the game's id (nil until known)
    name = "Obby",           -- the game's name (nil until known)
    location = "Frankfurt am Main, Hesse, DE", -- where the server is (nil until known)
    ping = 32,               -- your ping there in ms (nil until connected)
    since = 1795076918.2,    -- when you joined (unix seconds)
    link = "https://…",      -- a link that joins this server
}
```

### Doing things

| Function | Permission | Does |
|---|---|---|
| `mace.toast(title, body?)` | | A note at the bottom of the game for a few seconds |
| `mace.join(place, job?)` | `join` | This account joins the place (a new server), or that server of it |
| `mace.rejoin()` | `join` | Rejoins this server |
| `mace.hop({ sort? })` | `join` | Joins another server of this game with room; `sort` is `"most"` (fullest first, the default) or `"fewest"`. Returns `false` when there's none |
| `mace.servers({ place?, sort?, cursor? })` | | A page of the game's public servers (see below) |
| `mace.copy(text)` | `clipboard` | Puts text on the clipboard |
| `mace.accounts()` | `accounts` | Every account: `{ id, name, username, user, running, server }` (`server` as `mace.game()`) |
| `mace.start(account, link?)` | `accounts` | Starts an account (an id or name), joining `link` if given |
| `mace.stop(account)` | `accounts` | Stops an account's Roblox |
| `mace.gather()` | `accounts` | Brings every account to this account's server |

`mace.servers` returns:

```lua
{
    servers = { { id = "…", playing = 12, max = 20, ping = 32, location = "…" }, … },
    next = "cursor for the next page, or nil",
    error = nil, -- or why there's nothing (offline, Roblox asked to slow down)
}
```

`ping` and `location` are only known for servers one of your accounts has
been in. Roblox's own list doesn't say where a server is, or what your ping
to it would be.

### The bar

`mace.bar.button(definition)` adds the plugin's button (one per plugin):

```lua
local button = mace.bar.button({
    id = "main",         -- a name for it (default "main")
    icon = "star",       -- see Icons
    tip = "Hello",       -- shown on hover (default: the plugin's name)
    badge = 3,           -- a small number on it (optional)
    menu = function()    -- what its drop-down shows (optional)
        return mace.ui.menu({ title = "Hello" }, { mace.ui.text("Hi!") })
    end,
})

button:set({ badge = 0, tip = "Hi again", icon = "bolt" }) -- change it later (badge 0 hides it)
```

The bar has room for 4 plugin buttons, beside Mace's own.

- `mace.bar.close()` closes the bar, as Mace does after a join.
- `mace.refresh()` draws the open menu again (after a timer changed something).
  Mace already redraws it after any of its controls is used.

### Timers

```lua
local timer = mace.after(5, function() mace.toast("5 seconds later") end)
local ticker = mace.every(60, function() mace.toast("Another minute") end)
ticker:cancel()
```

`mace.every` runs at most once a second.

### Printing

`print(...)` writes to the account's log (`~/.local/share/mace/logs/ACCOUNT.log`),
as `plugin hello: …`.

## Events

```lua
mace.on("joined", function(server)
    mace.toast("Welcome to " .. (server.name or "a new server"))
end)
```

| Event     | When                                  | Gets |
|-----------|---------------------------------------|------|
| `joined`  | this account joined a server          | the server, as `mace.game()` |
| `left`    | it left the game (back home)          | |
| `opened`  | the bar opened                        | |
| `closed`  | the bar closed                        | |
| `focused` | the game window got the keyboard      | |

## Menus: `mace.ui`

A menu function returns `ui.menu(...)` with building blocks inside. Mace
draws them in its own style, so every plugin looks like part of Mace: there
are no colours or sizes to set, only names.

```lua
local ui = mace.ui

ui.menu({ title = "Title", subtitle = "a line under it" }, {
    ui.text("Hello", { style = "normal" }),          -- "normal", "subtle" or "title"
    ui.button("Go", {
        style = "primary",                           -- "primary", "secondary" or "danger"
        icon = "bolt",                               -- optional
        onClick = function() … end,
    }),
    ui.row({
        icon = "star",                               -- or image = "picture.png"
        title = "A row",
        subtitle = "with a line under it",
        onClick = function() … end,                  -- optional
        trailing = ui.button("X", { onClick = … }),  -- a button at its end (optional)
    }),
    ui.toggle("Sound", { value = true, onChange = function(on) … end }),
    ui.input({ placeholder = "Type here", value = "", onSubmit = function(text) … end }), -- Enter submits
    ui.stack({ ui.button("A"), ui.button("B") }, { direction = "row", gap = "small" }),  -- "row" or "column"; "small" or "medium"
    ui.space("medium"),                              -- "small" or "medium"
})
```

A plain string in a list is the same as `ui.text(string)`.

The menu is drawn when it opens, and again after each control is used, so
read your state inside the menu function and it stays current. A menu has at
most 300 blocks.

## Icons

Icons are named like [Material Symbols](https://fonts.google.com/icons) and
drawn rounded and filled, like the bar's own:

`add` `bolt` `bookmark` `bug_report` `chat` `check` `close` `content_copy`
`delete` `edit` `emoji_events` `explore` `extension` `favorite` `flag`
`games-recent` `group` `history` `home` `info` `link` `lock` `map`
`music_note` `notifications` `pause` `person` `play_arrow` `public` `refresh`
`rocket_launch` `schedule` `search` `select_window` `settings` `shield`
`shuffle` `skip_next` `sports_esports` `star` `sticky_note_2` `storage`
`swap_horiz` `sync` `timer` `visibility` `volume_off` `volume_up`

Or a PNG in the plugin's folder: `icon = "icon.png"`. As an icon only its
shape counts (it's drawn in the text's colour); as a row's `image` it keeps
its colours.

## Storage

```lua
mace.storage.set("count", 3)            -- numbers, text, booleans, tables
local count = mace.storage.get("count") -- nil when not set
mace.storage.set("count", nil)          -- removes it
local keys = mace.storage.keys()
```

Storage is the plugin's own, kept in `~/.local/share/mace/plugin-data/ID.json`,
and shared by every account's copy of the plugin. It survives reloads,
restarts and reinstalls. It holds up to 1 MB.

## Limits and errors

- A call into your plugin (an event, a timer, a click, a menu) may take 0.2 s
  of work; loading it, 1 s. Past that it's stopped with an error. Waiting on
  Roblox in `mace.servers` and `mace.hop` doesn't count.
- A plugin's memory is capped at 32 MB.
- An error shows as a toast ("Hello plugin failed: …", at most once a
  minute) and in full in the account's log. The plugin keeps running. One
  that fails to load is tried again when its files change.
- There's no `task.wait`. Use `mace.after` and `mace.every`.
