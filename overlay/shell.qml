import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Hyprland

// Mace's overlay: the in-game panel (Panel.qml) at the right edge of the
// focused Roblox window. Everything it does is a `mace` command, run by a
// long-running helper; the overlay keeps no state of its own.
//
//   qs -p <this dir> ipc call overlay toggle
//
// On Hyprland the panel's key (settings.json "overlay_key", SUPER + TAB by
// default) is bound only while a Roblox started by Mace has a window: windows
// opening and closing (and config reloads, which drop runtime binds) trigger
// `mace active`.
ShellRoot {
    id: root

    readonly property string mace: Quickshell.env("MACE_BIN") || "mace"
    property bool shown: false
    property var info: ({ "accounts": [], "focused": null, "window": null, "monitor": null })
    property string infoText: ""
    property real now: Date.now() / 1000
    property string flash: ""
    property bool picking: false
    property bool serverMore: false

    readonly property var me: info.accounts.find(a => a.id === info.focused) || null
    readonly property var server: me ? me.server : null
    readonly property var others: info.accounts.filter(a => a.id !== info.focused)
    // the alts in this session with you: running ones
    readonly property var withYou: others.filter(a => a.running)
    // the ones "Add alt" offers: not running, or in another server
    readonly property var addable: others.filter(a => !a.running || (server && (!a.server || a.server.job !== server.job)))
    readonly property var focusedScreen: {
        const name = Hyprland.focusedMonitor ? Hyprland.focusedMonitor.name : info.monitor;
        return Quickshell.screens.find(s => s.name === name) || Quickshell.screens[0];
    }

    function open() {
        refresh();
        picking = false;
        serverMore = false;
        shown = true;
    }
    function close() {
        shown = false;
    }
    function toggle() {
        shown ? close() : open();
    }
    function refresh() {
        send({ "cmd": "state" });
    }
    function act(args, message, closeAfter) {
        // starts from the panel aren't a choice of account for roblox.com's links
        if (args[0] === "start")
            args = args.concat(["--no-remember"]);
        request(args);
        if (message)
            say(message);
        if (closeAfter)
            close();
    }
    // show the result of an action at once; the next state from the helper confirms it
    function patch(accountId, fields) {
        const next = Object.assign({}, info);
        next.accounts = info.accounts.map(a => a.id === accountId ? Object.assign({}, a, fields) : a);
        info = next;
        infoText = "";
    }
    function toggleMute(account) {
        patch(account.id, { "muted": !account.muted });
        act(["mute", account.id], "");
    }
    function stopAccount(account) {
        patch(account.id, { "running": false, "server": null, "launch": null });
        act(["stop", account.id], "Stopping " + account.name + "…");
    }
    function startAccount(account, withServer) {
        patch(account.id, { "launch": "starting" });
        if (withServer && server)
            act(["start", account.id, me.id], account.name + " is joining…");
        else
            act(["start", account.id], "Starting " + account.name + "…");
    }
    // straight to Hyprland: no process in between
    function focusWindow(account) {
        close();
        if (onHyprland && account.address)
            Hyprland.dispatch("hl.dsp.focus({ window = \"address:" + account.address + "\" })");
        else
            request(["focus", account.id]);
    }
    function bare(address) {
        return (address || "").replace(/^0x/, "");
    }
    function say(text) {
        flash = text;
        flashTimer.restart();
    }
    function since(seconds) {
        const minutes = Math.max(0, Math.floor((now - seconds) / 60));
        if (minutes < 1)
            return "just joined";
        if (minutes < 60)
            return minutes + " min here";
        return Math.floor(minutes / 60) + " h " + (minutes % 60) + " min here";
    }
    function statusOf(a) {
        if (a.launch && ["preparing", "downloading", "starting", "updating"].includes(a.launch))
            return "starting…";
        if (!a.running)
            return "not running";
        if (!a.server)
            return "not in a game";
        if (server && a.server.job === server.job)
            return "here";
        return "in " + (a.server.name || "another game");
    }

    Theme {
        id: theme
    }

    Panel {}

    // -- the helper (mace overlay-helper): one process for every question and action

    property int nextId: 1
    property var waiting: ({})

    function send(message) {
        if (helper.running)
            helper.write(JSON.stringify(message) + "\n");
    }
    function request(args, done) {
        const id = nextId++;
        if (done) {
            const next = Object.assign({}, waiting);
            next[id] = done;
            waiting = next;
        }
        send({ "cmd": "run", "id": id, "args": args });
    }
    function received(line) {
        let message;
        try {
            message = JSON.parse(line);
        } catch (e) {
            return;
        }
        if (message.type === "state") {
            const text = JSON.stringify(message.state);
            // only when something changed: every new value rebuilds the bindings that read it
            if (text !== infoText) {
                infoText = text;
                info = message.state;
            }
        } else if (message.type === "done") {
            const done = waiting[message.id];
            if (done) {
                const next = Object.assign({}, waiting);
                delete next[message.id];
                waiting = next;
                done(message);
            } else if (!message.ok && message.error) {
                say(message.error);
            }
            refreshSoon.restart();
        }
    }

    Process {
        id: helper
        command: [root.mace, "overlay-helper"]
        running: true
        stdinEnabled: true
        stdout: SplitParser {
            onRead: line => root.received(line)
        }
        onRunningChanged: {
            if (!running)
                restartHelper.start();
        }
    }
    Timer {
        id: restartHelper
        interval: 1000
        onTriggered: helper.running = true
    }

    Timer {
        interval: 1000
        running: root.shown
        repeat: true
        onTriggered: {
            root.now = Date.now() / 1000;
            root.refresh();
        }
    }
    Timer {
        id: prefetch
        interval: 150
        onTriggered: root.refresh()
    }
    Timer {
        id: refreshSoon
        interval: 120
        onTriggered: root.refresh()
    }
    Timer {
        id: flashTimer
        interval: 2200
        onTriggered: root.flash = ""
    }

    IpcHandler {
        target: "overlay"

        function toggle(): void {
            root.toggle();
        }
        function open(): void {
            root.open();
        }
        function close(): void {
            root.close();
        }
    }

    // -- the panel's key, bound only while it's useful

    readonly property bool onHyprland: (Quickshell.env("HYPRLAND_INSTANCE_SIGNATURE") || "") !== ""
    property string overlayKey: "SUPER + TAB"
    property string boundKey: ""

    function lua(text) {
        return JSON.stringify(text);
    }
    // Hyprland animates layers on its own; for a panel that should just appear, that's lag
    function layerRules() {
        if (onHyprland)
            Quickshell.execDetached(["hyprctl", "eval", "hl.layer_rule({ match = { namespace = \"^mace-\" }, no_anim = true })"]);
    }
    function setBound(active) {
        if (!onHyprland)
            return;
        let code = "";
        if (boundKey !== "" && boundKey !== overlayKey)
            code += "hl.unbind(" + lua(boundKey) + ") ";
        if (active && overlayKey !== "") {
            const toggle = "qs -p '" + Quickshell.shellDir.replace(/'/g, "'\\''") + "' ipc call overlay toggle";
            code += "hl.unbind(" + lua(overlayKey) + ") hl.bind(" + lua(overlayKey) + ", hl.dsp.exec_cmd("
                + lua(toggle) + "), { description = \"Mace overlay\" })";
            boundKey = overlayKey;
        } else {
            // also after a restart, when a key from before may still be bound
            if (overlayKey !== "")
                code += "hl.unbind(" + lua(overlayKey) + ")";
            boundKey = "";
            close();
        }
        if (code !== "")
            Quickshell.execDetached(["hyprctl", "eval", code]);
    }

    FileView {
        path: (Quickshell.env("XDG_CONFIG_HOME") || (Quickshell.env("HOME") + "/.config")) + "/mace/settings.json"
        watchChanges: true
        printErrors: false
        onFileChanged: reload()
        onLoaded: {
            try {
                const key = JSON.parse(text()).overlay_key;
                if (typeof key === "string" && key !== root.overlayKey) {
                    root.overlayKey = key;
                    activeCheck.restart();
                }
            } catch (e) {}
        }
    }

    Timer {
        id: activeCheck
        interval: 500
        onTriggered: root.request(["active"], reply => root.setBound(reply.ok))
    }
    Connections {
        target: Hyprland
        enabled: root.onHyprland

        function onRawEvent(event) {
            if (["openwindow", "closewindow", "configreloaded"].includes(event.name)) {
                if (event.name === "configreloaded")
                    root.layerRules();
                activeCheck.restart();
            } else if (event.name === "activewindowv2") {
                const address = root.bare(event.data);
                // the panel belongs to one window: another one taking focus closes it
                if (root.shown && address !== "" && root.info.window && address !== root.bare(root.info.window.address))
                    root.close();
                // keep what the panel will show fresh while Roblox started by Mace runs
                if (!root.shown && root.boundKey !== "")
                    prefetch.restart();
            }
        }
    }
    Component.onCompleted: {
        layerRules();
        activeCheck.start();
    }
}
