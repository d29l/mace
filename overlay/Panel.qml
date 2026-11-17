import QtQuick
import QtQuick.Layouts
import Quickshell
import Quickshell.Wayland

// The in-game panel: who you're playing as, the frame rate, this server,
// the alts in the session with you, and "Add alt" to bring another one in.
PanelWindow {
    id: window

    readonly property int panelWidth: 372
    readonly property var win: root.info.window
    visible: root.shown
    screen: {
        const name = win ? win.monitor : root.info.monitor;
        return Quickshell.screens.find(s => s.name === name) || root.focusedScreen;
    }
    color: "transparent"
    exclusionMode: ExclusionMode.Ignore
    WlrLayershell.layer: WlrLayer.Overlay
    WlrLayershell.namespace: "mace-overlay"
    // the keyboard only when clicked: it never holds it from Roblox or any other window
    WlrLayershell.keyboardFocus: root.shown ? WlrKeyboardFocus.OnDemand : WlrKeyboardFocus.None

    // at the Roblox window's right edge; at the screen's when no Roblox window has focus
    anchors {
        top: true
        left: true
    }
    margins {
        left: win ? Math.max(0, win.x - win.monitor_x + win.width - panelWidth - 12)
                  : Math.max(0, (screen ? screen.width : 1920) - panelWidth - 12)
        top: win ? Math.max(0, win.y - win.monitor_y + 12) : 12
    }
    implicitWidth: panelWidth
    implicitHeight: Math.min(card.implicitHeight, win ? win.height - 24 : (screen ? screen.height - 24 : 1000))

    Rectangle {
        id: card

        width: window.panelWidth
        height: window.implicitHeight
        implicitHeight: content.implicitHeight + 32
        radius: 20
        color: theme.background
        clip: true
        focus: root.shown

        Keys.onEscapePressed: root.picking ? root.picking = false : root.close()

        Flickable {
            anchors.fill: parent
            anchors.margins: 16
            contentHeight: content.implicitHeight
            boundsBehavior: Flickable.StopAtBounds
            clip: true

            ColumnLayout {
                id: content
                width: parent.width
                spacing: 12

                // -- you
                RowLayout {
                    Layout.fillWidth: true
                    spacing: 12
                    Avatar {
                        size: 48
                        path: root.me ? (root.me.avatar || "") : ""
                        name: root.me ? root.me.name : "M"
                    }
                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: 0
                        Text {
                            Layout.fillWidth: true
                            text: root.me ? root.me.name : "No Roblox window has focus"
                            color: theme.text
                            font.pixelSize: 16
                            font.weight: Font.DemiBold
                            elide: Text.ElideRight
                        }
                        Text {
                            Layout.fillWidth: true
                            visible: root.me !== null && root.me.username !== ""
                            text: root.me ? "@" + root.me.username : ""
                            color: theme.subtext
                            font.pixelSize: 12
                            elide: Text.ElideRight
                        }
                    }
                    Chip {
                        visible: root.me !== null && root.me.fps !== null && root.me.fps !== undefined
                        text: root.me && root.me.fps !== null ? root.me.fps + " FPS" : ""
                    }
                    IconButton {
                        visible: root.me !== null && root.me.muted !== null && root.me.muted !== undefined
                        icon: root.me && root.me.muted ? "volume_off" : "volume_up"
                        tip: "Mute this account"
                        onClicked: root.toggleMute(root.me)
                    }
                }

                // -- this server: rejoin up front, the copy buttons behind ⋯
                Rectangle {
                    Layout.fillWidth: true
                    visible: root.server !== null
                    radius: 14
                    color: theme.surface
                    implicitHeight: serverBox.implicitHeight + 24

                    ColumnLayout {
                        id: serverBox
                        anchors.fill: parent
                        anchors.margins: 12
                        spacing: 10
                        RowLayout {
                            Layout.fillWidth: true
                            spacing: 6
                            ColumnLayout {
                                Layout.fillWidth: true
                                spacing: 2
                                Text {
                                    Layout.fillWidth: true
                                    text: root.server ? (root.server.name || "This game") : ""
                                    color: theme.text
                                    font.pixelSize: 15
                                    font.weight: Font.DemiBold
                                    elide: Text.ElideRight
                                }
                                Text {
                                    Layout.fillWidth: true
                                    text: root.server ? [root.server.location, root.since(root.server.since)].filter(x => x).join(" · ") : ""
                                    color: theme.subtext
                                    font.pixelSize: 12
                                    wrapMode: Text.Wrap
                                }
                            }
                            IconButton {
                                icon: root.serverMore ? "expand_less" : "more_horiz"
                                tip: "Copy the join link or job ID"
                                onClicked: root.serverMore = !root.serverMore
                            }
                        }
                        RowLayout {
                            Layout.fillWidth: true
                            visible: root.serverMore
                            spacing: 6
                            TextButton {
                                text: "Copy join link"
                                onClicked: root.act(["link", root.me.id], "Join link copied")
                            }
                            TextButton {
                                text: "Copy job ID"
                                onClicked: root.act(["job", root.me.id], "Job ID copied")
                            }
                        }
                        Pill {
                            Layout.fillWidth: true
                            raised: true
                            icon: "refresh"
                            label: "Rejoin"
                            onClicked: root.act(["rejoin", root.me.id], "Rejoining…", true)
                        }
                    }
                }

                // -- the alts with you, and a small + for more
                RowLayout {
                    Layout.fillWidth: true
                    visible: root.me !== null && (root.withYou.length > 0 || root.addable.length > 0)
                    Text {
                        Layout.fillWidth: true
                        text: root.withYou.length > 0 ? "With you" : "No alts with you"
                        color: theme.subtext
                        font.pixelSize: 12
                        font.weight: Font.DemiBold
                    }
                    IconButton {
                        visible: root.addable.length > 0
                        icon: root.picking ? "expand_less" : "person_add"
                        tip: root.server ? "Add an alt to this server" : "Start an alt"
                        onClicked: root.picking = !root.picking
                    }
                }
                // a count as the model: rows are only made or dropped when the number changes,
                // and otherwise just follow the data
                Repeater {
                    model: root.withYou.length
                    delegate: AltRow {
                        required property int index
                        account: root.withYou[index] || ({})
                    }
                }

                // the picker: everyone at once (in a server), or one account
                PickRow {
                    visible: root.picking && root.server !== null && root.addable.length > 1
                    icon: "group_add"
                    title: "Everyone"
                    subtitle: "Every other account joins this server"
                    onPicked: {
                        const shell = root;
                        shell.act(["gather", shell.me.id], "Calling every account here…");
                        shell.picking = false;
                    }
                }
                Repeater {
                    model: root.picking ? root.addable.length : 0
                    delegate: PickRow {
                        id: pick
                        required property int index
                        readonly property var alt: root.addable[index] || ({})
                        avatarPath: alt.avatar || ""
                        title: alt.name || ""
                        subtitle: root.statusOf(alt)
                        onPicked: {
                            // closing the picker destroys this row, so everything else comes first
                            const shell = root;
                            const chosen = pick.alt;
                            shell.startAccount(chosen, true);
                            shell.picking = false;
                        }
                    }
                }

                Text {
                    Layout.fillWidth: true
                    visible: root.flash !== ""
                    text: root.flash
                    color: theme.accent
                    font.pixelSize: 13
                    horizontalAlignment: Text.AlignHCenter
                }

                // a quiet footer: what this is
                RowLayout {
                    Layout.fillWidth: true
                    Layout.topMargin: 2
                    spacing: 6
                    Image {
                        source: Qt.resolvedUrl(Quickshell.shellDir + "/../data/mace.svg")
                        sourceSize.width: 32
                        sourceSize.height: 32
                        Layout.preferredWidth: 18
                        Layout.preferredHeight: 18
                        fillMode: Image.PreserveAspectFit
                        smooth: true
                        mipmap: true
                    }
                    Text {
                        Layout.fillWidth: true
                        text: "Mace"
                        color: theme.subtext
                        font.pixelSize: 11
                        font.weight: Font.DemiBold
                    }
                }
            }
        }
    }
}
