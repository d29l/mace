import QtQuick

// A small rounded label, e.g. the frame rate.
Rectangle {
    id: chip

    property string text: ""

    implicitHeight: 26
    implicitWidth: label.implicitWidth + 18
    radius: 13
    color: theme.accentSoft

    Text {
        id: label
        anchors.centerIn: parent
        text: chip.text
        color: theme.accent
        font.pixelSize: 12
        font.weight: Font.DemiBold
    }
}
