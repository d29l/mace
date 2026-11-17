import QtQuick
import Quickshell
import Quickshell.Io

// Colours: plain dark greys, the same everywhere. ~/.config/mace/theme.json
// can override any of them with Material colour roles (surface_container,
// on_surface, primary, ...), for anyone who wants it to match a theme.
QtObject {
    id: theme

    property var roles: ({})

    readonly property color background: roles.surface_container || "#242426"
    readonly property color surface: roles.surface_container_high || "#2f2f32"
    readonly property color surfaceHover: roles.surface_container_highest || "#3a3a3e"
    readonly property color surfaceRaised: Qt.lighter(surface, 1.18)
    readonly property color border: roles.outline_variant || "#3f3f44"
    readonly property color text: roles.on_surface || "#ededef"
    readonly property color subtext: roles.on_surface_variant || "#a1a1a8"
    readonly property color accent: roles.primary || "#d8d8dc"
    readonly property color accentSoft: roles.primary ? Qt.alpha(accent, 0.18) : "#3a3a3e"
    readonly property color onAccent: roles.on_primary || "#1c1c1e"
    readonly property color danger: roles.error || "#e66b6b"
    readonly property string iconFont: "Material Symbols Rounded"

    property FileView file: FileView {
        path: (Quickshell.env("XDG_CONFIG_HOME") || (Quickshell.env("HOME") + "/.config")) + "/mace/theme.json"
        watchChanges: true
        printErrors: false
        onFileChanged: reload()
        onLoaded: {
            try {
                theme.roles = JSON.parse(text());
            } catch (e) {
                theme.roles = {};
            }
        }
        onLoadFailed: theme.roles = {}
    }
}
