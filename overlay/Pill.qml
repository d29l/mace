import QtQuick
import QtQuick.Layouts

// A wide button with an icon and a label; `accent` fills it with the accent colour.
Rectangle {
    id: pill

    property string icon: ""
    property string label: ""
    property bool accent: false
    property bool raised: false  // on a surface already: one step lighter
    signal clicked

    implicitHeight: 38
    implicitWidth: row.implicitWidth + 24
    radius: 19
    color: accent ? (area.containsMouse ? Qt.lighter(theme.accent, 1.08) : theme.accent)
                  : (area.pressed ? Qt.lighter(theme.surfaceRaised, 1.15) : (area.containsMouse ? theme.surfaceHover : (raised ? theme.surfaceRaised : theme.surface)))

    RowLayout {
        id: row
        anchors.centerIn: parent
        spacing: 6
        Text {
            text: pill.icon
            font.family: theme.iconFont
            font.pixelSize: 18
            color: pill.accent ? theme.onAccent : theme.text
        }
        Text {
            text: pill.label
            font.pixelSize: 13
            font.weight: Font.Medium
            color: pill.accent ? theme.onAccent : theme.text
        }
    }

    MouseArea {
        id: area
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: pill.clicked()
    }
}
