import QtQuick

// A round icon button (Material Symbols name in `icon`).
Rectangle {
    id: button

    property string icon: ""
    property string tip: ""
    property bool danger: false
    signal clicked

    implicitWidth: 32
    implicitHeight: 32
    radius: 16
    color: area.pressed ? theme.surfaceRaised : (area.containsMouse ? theme.surfaceHover : "transparent")

    Text {
        anchors.centerIn: parent
        text: button.icon
        font.family: theme.iconFont
        font.pixelSize: 20
        color: button.danger ? theme.danger : theme.text
    }

    MouseArea {
        id: area
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: button.clicked()
    }
}
