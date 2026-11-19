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
  "permissions": [],
  "http": []
}
```

- `id` is the folder's name: lowercase letters, digits and `-`.
- `api` is the plugin API it was written for. This document is API 1. A
  plugin made for another API isn't loaded, and Mace Settings says why.
- `main` is the script Mace runs (`init.luau` if left out).
- `permissions`: see below.
- `http`: the sites it may reach (see [The web](#the-web)), like
  `"lrclib.net"`, or `"*.scdn.co"` for every site under scdn.co.

Plugins are off until you turn them on, in Mace Settings → Plugins or with
`mace plugins on ID`. Mace asks first, saying what the plugin may do, and
warns when it can reach the internet, naming the sites.

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
| `media`     | `mace.media` and the `media` event                       |

Calling one of these without its permission is an error that names the
permission it needs. The web needs no permission, only the sites listed in
`http`.

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

`mace.bar.button(definition)` gives the plugin its widget (one per plugin): its
menu, as a card. The bar has one button for every plugin, the puzzle piece;
its list turns each plugin's widget on screen and off. A widget on screen is
pinned: it stays while players play, where they put it, at the size they made
it.

```lua
local button = mace.bar.button({
    id = "main",         -- a name for it (default "main")
    icon = "star",       -- in the plugins list (see Icons)
    tip = "Hello",       -- its name in that list (default: the plugin's name)
    idle = false,        -- nothing to show now: its widget is hidden until it's false again
    menu = function(room) -- what its widget shows (optional; room: see Menus)
        return mace.ui.menu({ title = "Hello" }, { mace.ui.text("Hi!") })
    end,
})

button:set({ tip = "Hi again", icon = "bolt" })   -- change it later
button:set({ idle = true })                     -- music stopped: hide the widget for now
```

A plugin that only sometimes has something to show (a player, while nothing
plays) sets `idle`, and its widget goes away until it sets it back; the player
doesn't have to turn it off and on.

- `mace.bar.close()` closes the bar, as Mace does after a join.
- `mace.refresh()` draws the open menu again (after a timer changed something).
  Mace already redraws it after any of its controls is used, and after a
  request's answer comes. A redraw that comes out the same isn't sent to the
  game, so refreshing often is cheap.

While a widget is on screen, your menu function keeps being called as if its
menu were open.
While the mouse is free (not held by the game's camera), a pinned menu can
be clicked without opening the bar; typing into it needs the bar open.

### Timers

```lua
local timer = mace.after(5, function() mace.toast("5 seconds later") end)
local ticker = mace.every(60, function() mace.toast("Another minute") end)
ticker:cancel()
```

`mace.every` runs at most ten times a second (every 0.1 s).

### The web

```lua
mace.http.get("https://lrclib.net/api/search?q=hello", function(response)
    if response.ok then
        local results = mace.json.decode(response.body)
        mace.toast(`{#results} results`)
    else
        print(response.status, response.error)
    end
end)

mace.http.request({
    url = "https://example.com/api",
    method = "POST",                                -- GET, POST, PUT, PATCH, DELETE or HEAD
    headers = { ["Content-Type"] = "application/json" },
    body = mace.json.encode({ hello = true }),
}, function(response) … end)
```

The answer comes later, to the function: the game never waits on the
network. It's `{ ok, status, body, headers }`, or `{ ok = false, error }`
when there was no answer.

- Only `https://`, and only the sites in the plugin's `http`. Mace asks the
  player before letting a plugin with sites run. A site added to
  `plugin.json` later works once they've turned the plugin off and on again.
- Redirects aren't followed (they could lead anywhere).
- At most 6 requests at once and 120 a minute; answers up to 2 MB; 10 s each.

`mace.json.encode(value)` and `mace.json.decode(text)` turn tables into JSON
text and back (JSON's `null` is `nil`).

### Media

With the `media` permission, a plugin sees what's playing on the desktop
(Spotify, a browser, any player with media keys) and controls it:

```lua
local now = mace.media.current() -- nil when nothing plays
-- { player = "spotify", title, artist, album, art = "https://…", length = 215.3,
--   position = 31.2, playing = true, status = "Playing" }

mace.media.playPause()
mace.media.next()
mace.media.previous()
mace.media.seek(60)        -- to one minute in
mace.media.players()       -- { "spotify", "brave.instance2453" }
mace.media.volume()        -- 0.65 (0 to 1), or nil
mace.media.setVolume(0.4)
```

`length` and `position` are in seconds. Each function takes a player's name
last, to pick one (`mace.media.current("spotify")`); without it, the one
playing is used. The `media` event comes when the song changes or it's paused
or played, with `current()` (or `nil` when the player went away).

The volume is the player's own sound stream (PipeWire or PulseAudio), so it
works with players that ignore volume over MPRIS, Spotify among them; it
changes only that player, not the rest of the desktop.

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
| `media`   | the song changed, or was paused or played (`media` permission) | `mace.media.current()` |

## Menus: `mace.ui`

A menu function returns `ui.menu(...)` with building blocks inside. Mace
draws them in its own style, so every plugin looks like part of Mace: there
are no colours to set, only names, and sizes come from names too.

```lua
local ui = mace.ui

ui.menu({ title = "Title", subtitle = "a line under it" }, {
    ui.text("Hello", {
        style = "normal",                            -- "normal", "subtle" or "title" (bold)
        size = "large",                              -- "small", "normal", "large" or "huge" (optional)
        lines = 1,                                   -- at most this many, cut with … (optional)
    }),
    ui.button("Go", {
        style = "primary",                           -- "primary", "secondary", "plain" or "danger"
        icon = "bolt",                               -- optional
        size = "medium",                             -- "small", "medium", "large" or "huge"
        onClick = function() … end,
    }),
    ui.button("", { icon = "skip_next", style = "plain" }), -- no label: a round icon button
    ui.row({
        icon = "star",                               -- or image = "picture.png"
        title = "A row",
        subtitle = "with a line under it",
        onClick = function() … end,                  -- optional
        trailing = ui.button("X", { onClick = … }),  -- a button at its end (optional)
    }),
    ui.toggle("Sound", { value = true, onChange = function(on) … end }),
    ui.input({ placeholder = "Type here", value = "", onSubmit = function(text) … end }), -- Enter submits
    ui.stack({ ui.button("A"), ui.button("B") }, {
        direction = "row",                           -- "row" or "column" (the default)
        gap = "small",                               -- "none", "small", "medium" or "large"
        align = "center",                            -- "start", "center" or "end"
        grow = false,                                -- in a row: take the room the others leave
    }),
    ui.overlay({ ui.image(cover, { size = "fill" }), controls }, { reveal = "hover" }), -- see below
    ui.space("medium"),                              -- "small", "medium" or "large"
    ui.progress(31, 215, { rate = 1, times = true, onSeek = function(to) … end }), -- see below
    ui.image("https://i.scdn.co/image/…", { size = "medium" }), -- see below
    ui.slider(0.65, { min = 0, max = 1, icon = "volume_up", onChange = function(v) … end }),
})
```

A plain string in a list is the same as `ui.text(string)`.

**Laying things out.** A column puts blocks under each other; a row puts
them side by side, lined up across their middles. In a row, blocks that fill
(a progress bar, a text field, a row, a toggle, a picture with `size =
"fill"`, or a stack with `grow = true`) share the room the others leave, so
`ui.stack({ cover, ui.stack({ title, artist }, { grow = true }), buttons }, {
direction = "row" })` puts the buttons at the far end. `align = "center"`
centres a row's blocks (or, in a column, each block).

**On top of each other.** `ui.overlay(blocks, { align, reveal })` draws the
first block, then the others over it (`align`: "start", "center" or "end",
top to bottom). With `reveal = "hover"`, the others only show while the
pointer is over it, and the first is darkened under them: controls on a cover.

**Progress.** `ui.progress(value, max, { rate, times, onSeek })` is a bar
from 0 to `max`, at `value` when drawn. It moves on by `rate` a second by
itself, smoothly, so a song's progress needs no redrawing: `rate = 1` while
it plays, `0` when it's paused. With `times = true` it shows `0:31` and
`3:35` under it. With `onSeek`, clicking or dragging it calls that with the
value picked.

**Pictures.** `ui.image(picture, { size })` and a row's `image` are a PNG in
the plugin's folder, or an `https://` picture from one of its sites (any
common format; drawn square, cropped to its middle). `size` is "small" (48),
"medium" (96, the default), "large" (160), "fill" (as wide as there's room
for), or a number of points.

**Sliders.** `ui.slider(value, { min, max, icon, onChange })` is a bar with a
knob, from `min` to `max` (0 and 1 when not given). Dragging it calls
`onChange` with the new value, every so often while dragging and where it's
let go.

**What pops out.** `ui.menu({ below = { … } }, blocks)`: the blocks in
`below` pop out under the card, in a card of their own as tall as they need,
whatever the size of the card above (lyrics or a volume slider under a
player). Leave it empty and nothing pops out.

**No title.** A menu without a title has no header, and its card has thin
margins: the grip to move it (top middle) and its pin (top left) show over it
while it's pointed at. For strips as thin as a mini-player's.

**Its size.** The menu function gets the room its menu has, `{ width,
height }` in points, and is called again when that changes (the player
resized it). `height` is 0 when the menu may be as tall as it needs (a
drop-down, or a pinned card not resized yet). Use it to pick a layout:

```lua
menu = function(room)
    if room.height > 0 and room.height >= room.width then
        return tall(room)  -- e.g. a big cover with controls on it
    end
    return strip(room)     -- e.g. a cover, the words, the controls in a row
end
```

The menu is drawn when it opens, and again after each control is used, so
read your state inside the menu function and it stays current. A menu has at
most 300 blocks.

## Icons

Icons are named like [Material Symbols](https://fonts.google.com/icons) and
drawn rounded and filled, like the bar's own:

`add` `album` `bolt` `bookmark` `bug_report` `chat` `check` `close`
`content_copy` `delete` `edit` `emoji_events` `explore` `extension`
`favorite` `flag` `games-recent` `group` `history` `home` `info` `link`
`lock` `lyrics` `map` `music_note` `notifications` `pause` `person`
`play_arrow` `public` `push_pin` `refresh` `rocket_launch` `schedule`
`search` `select_window` `settings` `shield` `shuffle` `skip_next`
`skip_previous` `sports_esports` `star` `sticky_note_2` `storage`
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
  Roblox in `mace.servers` and `mace.hop`, or on `mace.media`, doesn't count.
- A plugin's memory is capped at 32 MB.
- An error shows as a toast ("Hello plugin failed: …", at most once a
  minute) and in full in the account's log. The plugin keeps running. One
  that fails to load is tried again when its files change.
- There's no `task.wait`. Use `mace.after` and `mace.every`.
