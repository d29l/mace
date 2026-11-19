//! `mace.media`: what's playing on the desktop, and its controls, through
//! MPRIS (the D-Bus interface Spotify, browsers and most players offer). No
//! logins: it's the same thing a desktop's media keys use.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

use gtk::gio;
use gtk::glib::{self, Variant, prelude::*};
use serde_json::{Value, json};

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
/// D-Bus calls are local and quick; one that isn't is given up on.
const TIMEOUT_MS: i32 = 500;

fn bus() -> Option<gio::DBusConnection> {
    thread_local! {
        static BUS: Option<gio::DBusConnection> = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok();
    }
    BUS.with(Clone::clone)
}

fn call(bus: &gio::DBusConnection, name: &str, path: &str, interface: &str, method: &str, args: Option<&Variant>)
        -> Option<Variant> {
    bus.call_sync(Some(name), path, interface, method, args, None, gio::DBusCallFlags::NONE, TIMEOUT_MS,
                  gio::Cancellable::NONE).ok()
}

/// The players there are, by their D-Bus name after `org.mpris.MediaPlayer2.`
/// (`spotify`, `brave.instance2453`); playerctld only mirrors the others.
pub fn players() -> Vec<String> {
    let Some(bus) = bus() else { return Vec::new() };
    let Some(reply) = call(&bus, "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "ListNames", None)
    else {
        return Vec::new();
    };
    let names: Vec<String> = reply.child_value(0).get().unwrap_or_default();
    let mut found: Vec<String> = names.into_iter().filter_map(|n| n.strip_prefix(PREFIX).map(String::from))
        .filter(|n| n != "playerctld").collect();
    found.sort();
    found
}

/// What a variant holds, when it's a box around another value (players differ in that).
fn unbox(value: Variant) -> Variant {
    if value.is_type(glib::VariantTy::VARIANT) { value.as_variant().unwrap_or(value) } else { value }
}

/// A value in an `a{sv}` dictionary.
fn lookup(dict: &Variant, key: &str) -> Option<Variant> {
    glib::VariantDict::new(Some(dict)).lookup_value(key, None)
}

fn text(dict: &Variant, key: &str) -> Option<String> {
    let value = lookup(dict, key)?;
    // some players box their values in a variant of their own
    let value = unbox(value);
    if let Some(s) = value.str() {
        return Some(s.to_string()).filter(|s| !s.is_empty());
    }
    // artists are a list
    let list: Vec<String> = value.get()?;
    Some(list.join(", ")).filter(|s| !s.is_empty())
}

fn number(dict: &Variant, key: &str) -> Option<i64> {
    let value = lookup(dict, key)?;
    let value = unbox(value);
    value.get::<i64>().or_else(|| value.get::<u64>().map(|n| n as i64)).or_else(|| value.get::<i32>().map(i64::from))
}

/// Where each track was seen to start, for players that report position 0.
struct Seen {
    track: String,
    playing: bool,
    /// Seconds into the track at `at`.
    position: f64,
    at: Instant,
}

static SEEN: LazyLock<Mutex<HashMap<String, Seen>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn state(bus: &gio::DBusConnection, player: &str) -> Option<Value> {
    let args = (PLAYER,).to_variant();
    let reply = call(bus, &format!("{PREFIX}{player}"), PATH, "org.freedesktop.DBus.Properties", "GetAll", Some(&args))?;
    let props = reply.child_value(0);
    let status = text(&props, "PlaybackStatus").unwrap_or_default();
    let meta = lookup(&props, "Metadata")?;
    let meta = unbox(meta);
    let title = text(&meta, "xesam:title");
    let length = number(&meta, "mpris:length").map(|us| us as f64 / 1e6);
    let reported = number(&props, "Position").map(|us| us as f64 / 1e6);
    let playing = status == "Playing";
    let track = text(&meta, "mpris:trackid").or_else(|| title.clone()).unwrap_or_default();
    // a position that stays at 0 while playing is a player that doesn't say: count from the track's start
    let position = {
        let mut seen = SEEN.lock().unwrap();
        let entry = seen.entry(player.to_string()).or_insert_with(|| Seen {
            track: track.clone(), playing, position: reported.unwrap_or(0.0), at: Instant::now(),
        });
        let estimate = entry.position + if entry.playing { entry.at.elapsed().as_secs_f64() } else { 0.0 };
        let position = match reported {
            Some(p) if p > 0.0 || !playing => p,
            _ if entry.track == track => estimate,
            _ => 0.0,
        };
        *entry = Seen { track, playing, position, at: Instant::now() };
        position
    };
    Some(json!({
        "player": player,
        "title": title,
        "artist": text(&meta, "xesam:artist"),
        "album": text(&meta, "xesam:album"),
        "art": text(&meta, "mpris:artUrl"),
        "length": length,
        "position": length.map_or(position, |l| position.min(l)),
        "playing": playing,
        "status": status,
    }))
}

/// The player that matters: the one named (or whose name starts so), else
/// one that's playing, else the first.
fn choose(bus: &gio::DBusConnection, hint: Option<&str>) -> Option<(String, Value)> {
    let players = players();
    if let Some(hint) = hint {
        let name = players.iter().find(|p| *p == hint || p.starts_with(&format!("{hint}.")))?;
        return state(bus, name).map(|s| (name.clone(), s));
    }
    let states: Vec<(String, Value)> = players.iter().filter_map(|p| state(bus, p).map(|s| (p.clone(), s))).collect();
    states.iter().find(|(_, s)| s["playing"] == true).or_else(|| states.first()).cloned()
}

/// What's playing (see `state`), or None when no player is there.
pub fn current(hint: Option<&str>) -> Option<Value> {
    let bus = bus()?;
    choose(&bus, hint).map(|(_, s)| s)
}

/// `PlayPause`, `Play`, `Pause`, `Next`, `Previous`; or `seek` to a time (seconds).
pub fn control(hint: Option<&str>, action: &str, seconds: Option<f64>) -> bool {
    let Some(bus) = bus() else { return false };
    let Some((player, _)) = choose(&bus, hint) else { return false };
    let name = format!("{PREFIX}{player}");
    if action == "seek" {
        // SetPosition wants the track's id
        let args = (PLAYER, "Metadata").to_variant();
        let id = call(&bus, &name, PATH, "org.freedesktop.DBus.Properties", "Get", Some(&args))
            .map(|r| r.child_value(0)).map(unbox)
            .and_then(|meta| lookup(&meta, "mpris:trackid"))
            .map(unbox)
            .and_then(|v| v.str().map(String::from));
        let Some(id) = id.and_then(|id| glib::variant::ObjectPath::try_from(id).ok()) else { return false };
        let micros = (seconds.unwrap_or(0.0).max(0.0) * 1e6) as i64;
        return call(&bus, &name, PATH, PLAYER, "SetPosition", Some(&(id, micros).to_variant())).is_some();
    }
    call(&bus, &name, PATH, PLAYER, action, None).is_some()
}

/// The player's own sound stream (PipeWire or PulseAudio), found by its
/// program's name: `spotify`, or `brave` for `brave.instance2453`.
fn stream(player: &str) -> Option<Value> {
    let name = player.split('.').next()?.to_lowercase();
    crate::processes::sink_inputs().into_iter().find(|s| {
        let property = |key: &str| s.pointer(&format!("/properties/{key}")).and_then(Value::as_str).unwrap_or("").to_lowercase();
        property("application.process.binary").contains(&name) || property("application.name").contains(&name)
    })
}

/// A stream's volume, 0 to 1 (its first channel's; 1 is 100%).
fn stream_volume(stream: &Value) -> Option<f64> {
    let first = stream.get("volume")?.as_object()?.values().next()?;
    Some(first.get("value")?.as_f64()? / 65536.0)
}

/// How loud the player is, 0 to 1: its sound stream's volume, or what it says
/// over MPRIS (some players, Spotify among them, don't act on that).
pub fn volume(hint: Option<&str>) -> Option<f64> {
    let bus = bus()?;
    let (player, _) = choose(&bus, hint)?;
    if let Some(level) = stream(&player).as_ref().and_then(stream_volume) {
        return Some(level);
    }
    let args = (PLAYER, "Volume").to_variant();
    let reply = call(&bus, &format!("{PREFIX}{player}"), PATH, "org.freedesktop.DBus.Properties", "Get", Some(&args))?;
    unbox(reply.child_value(0)).get::<f64>()
}

/// Set how loud the player is, 0 to 1: its sound stream when it has one, else over MPRIS.
pub fn set_volume(hint: Option<&str>, level: f64) -> bool {
    let Some(bus) = bus() else { return false };
    let Some((player, _)) = choose(&bus, hint) else { return false };
    let level = level.clamp(0.0, 1.0);
    if let Some(index) = stream(&player).and_then(|s| s.get("index").and_then(Value::as_u64)) {
        return std::process::Command::new("pactl")
            .args(["set-sink-input-volume", &index.to_string(), &format!("{}%", (level * 100.0).round())])
            .status().is_ok_and(|s| s.success());
    }
    let args = (PLAYER, "Volume", level.to_variant()).to_variant();
    call(&bus, &format!("{PREFIX}{player}"), PATH, "org.freedesktop.DBus.Properties", "Set", Some(&args)).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    const INTERFACE: &str = r#"<node><interface name="org.mpris.MediaPlayer2.Player">
        <method name="PlayPause"/><method name="Next"/><method name="Previous"/><method name="Play"/><method name="Pause"/>
        <method name="SetPosition"><arg type="o" direction="in"/><arg type="x" direction="in"/></method>
        <property name="PlaybackStatus" type="s" access="read"/>
        <property name="Metadata" type="a{sv}" access="read"/>
        <property name="Position" type="x" access="read"/>
        <property name="Volume" type="d" access="readwrite"/>
    </interface></node>"#;

    /// A player on a private bus, the way Spotify answers: (the bus, how often it was told to play or pause).
    fn fake_player() -> (std::process::Child, String, Arc<AtomicU32>) {
        let mut daemon = std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null())
            .spawn().expect("dbus-daemon");
        let mut address = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(daemon.stdout.take().unwrap()), &mut address).unwrap();
        let address = address.trim().to_string();
        let toggles = Arc::new(AtomicU32::new(0));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        {
            let (address, toggles) = (address.clone(), toggles.clone());
            std::thread::spawn(move || {
                let context = glib::MainContext::new();
                let _guard = context.acquire().unwrap();
                context.with_thread_default(|| {
                    let flags = gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION;
                    let bus = gio::DBusConnection::for_address_sync(&address, flags, None, gio::Cancellable::NONE).unwrap();
                    let info = gio::DBusNodeInfo::for_xml(INTERFACE).unwrap();
                    let interface = info.lookup_interface(PLAYER).unwrap();
                    let level = std::rc::Rc::new(std::cell::Cell::new(0.8f64));
                    let level_set = level.clone();
                    let _id = bus.register_object(PATH, &interface)
                        .method_call(move |_, _, _, _, method, _, invocation| {
                            if method == "PlayPause" {
                                toggles.fetch_add(1, Ordering::Relaxed);
                            }
                            invocation.return_value(None);
                        })
                        .set_property(move |_, _, _, _, name, value| {
                            if name == "Volume" && let Some(v) = value.get::<f64>() {
                                level_set.set(v);
                            }
                            true
                        })
                        .property(move |_, _, _, _, name| match name {
                            "PlaybackStatus" => "Playing".to_variant(),
                            "Volume" => level.get().to_variant(),
                            "Position" => 31_200_000i64.to_variant(),
                            _ => {
                                let meta = glib::VariantDict::new(None);
                                meta.insert_value("xesam:title", &"Blinding Lights".to_variant());
                                meta.insert_value("xesam:artist", &vec!["The Weeknd", "Someone"].to_variant());
                                meta.insert_value("xesam:album", &"After Hours".to_variant());
                                meta.insert_value("mpris:length", &200_040_000u64.to_variant());
                                meta.insert_value("mpris:artUrl", &"https://i.scdn.co/image/ab67".to_variant());
                                let id = glib::variant::ObjectPath::try_from("/com/spotify/track/0VjIjW4GlUZAMYd2vXMi3b".to_string()).unwrap();
                                meta.insert_value("mpris:trackid", &id.to_variant());
                                meta.end()
                            }
                        })
                        .build().unwrap();
                    bus.call_sync(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
                                  Some(&("org.mpris.MediaPlayer2.spotify", 0u32).to_variant()), None,
                                  gio::DBusCallFlags::NONE, 1000, gio::Cancellable::NONE).unwrap();
                    ready_tx.send(()).unwrap();
                    loop {
                        context.iteration(true);
                    }
                }).unwrap();
            });
        }
        ready_rx.recv().unwrap();
        (daemon, address, toggles)
    }

    #[test]
    fn what_spotify_says_is_read_and_its_buttons_work() {
        let _home = crate::testenv::TempHome::new();
        let (mut daemon, address, toggles) = fake_player();
        let before = std::env::var_os("DBUS_SESSION_BUS_ADDRESS");
        // SAFETY: TempHome holds the environment lock.
        unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &address) };
        // on a thread of its own, so the bus it finds is this one
        // no sound server: the volume goes over MPRIS (and nothing real is touched)
        let pulse = std::env::var_os("PULSE_SERVER");
        unsafe { std::env::set_var("PULSE_SERVER", "unix:/nonexistent/mace-test") };
        let (now, players, toggled, volumes) = std::thread::spawn(|| {
            let before = volume(Some("spotify"));
            let set = set_volume(Some("spotify"), 0.4);
            (current(Some("spotify")), players(), control(Some("spotify"), "PlayPause", None), (before, set, volume(Some("spotify"))))
        }).join().unwrap();
        unsafe {
            match pulse {
                Some(v) => std::env::set_var("PULSE_SERVER", v),
                None => std::env::remove_var("PULSE_SERVER"),
            }
        }
        assert_eq!(volumes, (Some(0.8), true, Some(0.4)));
        unsafe {
            match before {
                Some(v) => std::env::set_var("DBUS_SESSION_BUS_ADDRESS", v),
                None => std::env::remove_var("DBUS_SESSION_BUS_ADDRESS"),
            }
        }
        let _ = daemon.kill();
        assert_eq!(players, ["spotify"]);
        let now = now.expect("a song");
        assert_eq!(now["title"], "Blinding Lights");
        assert_eq!(now["artist"], "The Weeknd, Someone");
        assert_eq!(now["album"], "After Hours");
        assert_eq!(now["art"], "https://i.scdn.co/image/ab67");
        assert_eq!((now["length"].as_f64(), now["position"].as_f64(), now["playing"].as_bool()),
                   (Some(200.04), Some(31.2), Some(true)));
        assert!(toggled);
        assert_eq!(toggles.load(Ordering::Relaxed), 1);
    }
}
