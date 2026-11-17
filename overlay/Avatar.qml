import QtQuick
import QtQuick.Effects

// A round avatar: the account's Roblox headshot (a file path), or its initial
// until there is one.
Item {
    id: avatar

    property string path: ""
    property string name: ""
    property int size: 36
    property bool dim: false

    implicitWidth: size
    implicitHeight: size
    opacity: dim ? 0.55 : 1

    Rectangle {
        anchors.fill: parent
        radius: width / 2
        color: theme.surface
    }

    Text {
        anchors.centerIn: parent
        visible: image.status !== Image.Ready
        text: (avatar.name || "?").charAt(0).toUpperCase()
        color: theme.subtext
        font.pixelSize: Math.round(avatar.size * 0.42)
        font.weight: Font.DemiBold
    }

    Image {
        id: image
        anchors.fill: parent
        visible: false
        source: avatar.path ? "file://" + avatar.path : ""
        sourceSize.width: avatar.size * 2
        sourceSize.height: avatar.size * 2
        smooth: true
        mipmap: true
        asynchronous: true
    }

    Item {
        id: mask
        anchors.fill: parent
        visible: false
        layer.enabled: true
        Rectangle {
            anchors.fill: parent
            radius: width / 2
            color: "black"
        }
    }

    MultiEffect {
        anchors.fill: parent
        source: image
        visible: image.status === Image.Ready
        maskEnabled: true
        maskSource: mask
    }
}
