import QtQuick

// A quiet text-only button.
Rectangle {
    id: button

    property string text: ""
    signal clicked

    implicitHeight: 30
    implicitWidth: label.implicitWidth + 20
    radius: 15
    color: area.pressed ? theme.surfaceRaised : (area.containsMouse ? theme.surfaceHover : "transparent")
    border.color: theme.border
    border.width: 1

    Text {
        id: label
        anchors.centerIn: parent
        text: button.text
        color: theme.text
        font.pixelSize: 12
    }
    MouseArea {
        id: area
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: button.clicked()
    }
}
