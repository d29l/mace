#!/bin/sh
# Run a Mace UI (the window, the loading window, or a game with the in-game
# panel: dev/standin) in a headless sway and take
# screenshots, away from the real session: its own runtime dir and Wayland
# socket, and no route to the audio system.
#
#   dev/headless.sh OUT_PREFIX "STEP" "STEP" ... -- COMMAND...
#
# COMMAND starts in the background inside the compositor; then the steps run
# in order, each one quoted string:
#   "sleep S"            wait
#   "shot NAME"          screenshot to OUT_PREFIX-NAME.png
#   "click X Y"          left click (needs $WLRCTL: a wlrctl that clicks after a move)
#   "key TEXT"           type text (needs $WLRCTL)
#   "run CMD..."         run a shell command inside the compositor's environment
#
# MACE_HEADLESS_SIZE sets the output size (default 1600x1000). The caller's
# XDG_CONFIG_HOME / XDG_DATA_HOME / XDG_CACHE_HOME pass through, so point them
# at test data first.
set -eu
out=$1; shift
tmp=$(mktemp -d)
: > "$tmp/steps"
while [ $# -gt 0 ] && [ "$1" != "--" ]; do printf '%s\n' "$1" >> "$tmp/steps"; shift; done
[ "${1:-}" = "--" ] && shift
[ $# -gt 0 ] || { echo "usage: $0 OUT \"STEP\"... -- COMMAND..." >&2; exit 2; }

sway_pid= app_pid=
cleanup() {
    [ -n "$app_pid" ] && kill "$app_pid" 2>/dev/null || true
    [ -n "$sway_pid" ] && kill "$sway_pid" 2>/dev/null || true
    wait 2>/dev/null || true
    rm -rf "$tmp"
}
trap cleanup EXIT INT TERM
mkdir -p "$tmp/run"
chmod 700 "$tmp/run"
printf 'output HEADLESS-1 resolution %s bg #1e1e24 solid_color\ndefault_border none\nseat seat0 hide_cursor 0\n' \
    "${MACE_HEADLESS_SIZE:-1600x1000}" > "$tmp/sway.conf"

env -i HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$tmp/run" WLR_BACKENDS=headless \
    WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=gles2 sway -c "$tmp/sway.conf" >"$tmp/sway.log" 2>&1 &
sway_pid=$!
sock=
for _ in $(seq 50); do
    sock=$(ls "$tmp/run" | grep -m1 '^wayland-[0-9]*$' || true)
    [ -n "$sock" ] && break
    sleep 0.1
done
[ -n "$sock" ] || { echo "sway didn't start:" >&2; cat "$tmp/sway.log" >&2; exit 1; }

# nothing started here can make a sound or reach the real session
export PULSE_SERVER="unix:/nonexistent/mace-headless" PIPEWIRE_REMOTE="mace-headless-none" \
    PIPEWIRE_RUNTIME_DIR="$tmp/none"
# nor reach the real desktop: no session bus (notifications could play a sound)
export DBUS_SESSION_BUS_ADDRESS="unix:path=/nonexistent/mace-headless"
printf 'pcm.!default { type null }\nctl.!default { type hw card 0 }\n' > "$tmp/asound.conf"
export ALSA_CONFIG_PATH="$tmp/asound.conf"
export XDG_RUNTIME_DIR="$tmp/run" WAYLAND_DISPLAY="$sock" GDK_BACKEND=wayland QT_QPA_PLATFORM=wayland
unset DISPLAY HYPRLAND_INSTANCE_SIGNATURE SWAYSOCK

"$@" >"$tmp/app.log" 2>&1 &
app_pid=$!
sleep 2

wlrctl=${WLRCTL:-wlrctl}
while IFS= read -r step; do
    set -- $step
    case $1 in
        sleep) sleep "$2" ;;
        shot) grim -c "$out-$2.png" ;;
        click) $wlrctl pointer move -20000 -20000; WLRCTL_CLICK=1 $wlrctl pointer move "$2" "$3" ;;
        key) shift; $wlrctl keyboard type "$*" ;;
        run) shift; sh -c "$*" ;;
    esac
done < "$tmp/steps"

if grep -iE 'Traceback|Error|CRITICAL|warning' "$tmp/app.log" >/dev/null; then
    sed 's/^/  app: /' "$tmp/app.log" | tail -30 >&2
fi
