import QtQuick
import QtQuick.Layouts

// One choice in the "add an alt" picker: an avatar (or an icon), two lines, a +.
Rectangle {
    id: pickRow

    property string avatarPath: ""
    property string icon: ""
    property string title: ""
    property string subtitle: ""
    signal picked

    Layout.fillWidth: true
    implicitHeight: 52
    radius: 14
    color: area.pressed ? theme.surfaceRaised : (area.containsMouse ? theme.surfaceHover : theme.surface)

    RowLayout {
        anchors.fill: parent
        anchors.leftMargin: 10
        anchors.rightMargin: 12
        spacing: 10
        Avatar {
            visible: pickRow.icon === ""
            size: 34
            path: pickRow.avatarPath
            name: pickRow.title
        }
        Rectangle {
            visible: pickRow.icon !== ""
            implicitWidth: 34
            implicitHeight: 34
            radius: 17
            color: theme.accentSoft
            Text {
                anchors.centerIn: parent
                text: pickRow.icon
                font.family: theme.iconFont
                font.pixelSize: 20
                color: theme.accent
            }
        }
        ColumnLayout {
            Layout.fillWidth: true
            spacing: 0
            Text {
                Layout.fillWidth: true
                text: pickRow.title
                color: theme.text
                font.pixelSize: 14
                elide: Text.ElideRight
            }
            Text {
                Layout.fillWidth: true
                text: pickRow.subtitle
                color: theme.subtext
                font.pixelSize: 12
                elide: Text.ElideRight
            }
        }
        Text {
            text: "add"
            font.family: theme.iconFont
            font.pixelSize: 22
            color: theme.accent
        }
    }
    MouseArea {
        id: area
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: pickRow.picked()
    }
}
