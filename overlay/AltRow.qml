import QtQuick
import QtQuick.Layouts

// An alt in the session: its headshot, where it is, and what can be done with it.
RowLayout {
    id: row

    property var account: ({})
    readonly property bool here: root.statusOf(account) === "here"

    Layout.fillWidth: true
    spacing: 10

    Avatar {
        size: 36
        path: row.account.avatar || ""
        name: row.account.name
    }
    ColumnLayout {
        Layout.fillWidth: true
        spacing: 0
        Text {
            Layout.fillWidth: true
            text: row.account.name
            color: theme.text
            font.pixelSize: 14
            elide: Text.ElideRight
        }
        Text {
            Layout.fillWidth: true
            text: root.statusOf(row.account) + (row.account.fps ? " · " + row.account.fps + " FPS" : "")
            color: row.here ? theme.accent : theme.subtext
            font.pixelSize: 12
            elide: Text.ElideRight
        }
    }
    IconButton {
        visible: row.account.muted !== null && row.account.muted !== undefined
        icon: row.account.muted ? "volume_off" : "volume_up"
        tip: "Mute"
        onClicked: root.toggleMute(row.account)
    }
    IconButton {
        icon: "open_in_new"
        tip: "Switch to its window"
        onClicked: root.focusWindow(row.account)
    }
    IconButton {
        icon: "close"
        danger: true
        tip: "Stop"
        onClicked: root.stopAccount(row.account)
    }
}
