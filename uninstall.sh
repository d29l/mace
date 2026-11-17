#!/bin/sh
# Remove Mace. Accounts, their prefixes and downloaded builds are kept unless
# --purge is given; prefixes Mace didn't make (Nutmeg's, say) are never deleted.
set -eu
purge=
[ "${1:-}" = "--purge" ] && purge=1
config=${XDG_CONFIG_HOME:-$HOME/.config}
data=${XDG_DATA_HOME:-$HOME/.local/share}
for unit in mace-toasts.service mace-overlay.service; do
    if [ -f "$config/systemd/user/$unit" ]; then
        systemctl --user disable --now "$unit" 2>/dev/null || true
        rm -f "$config/systemd/user/$unit"
    fi
done
systemctl --user daemon-reload 2>/dev/null || true
[ -x "$HOME/.local/bin/mace" ] && "$HOME/.local/bin/mace" integrate --undo || true
rm -f "$HOME/.local/bin/mace"
rm -rf "$data/mace/app"
if [ -n "$purge" ]; then
    rm -rf "$data/mace" "$config/mace" "${XDG_CACHE_HOME:-$HOME/.cache}/mace"
    echo "removed Mace, its accounts' Roblox copies, the prefixes it made, and its settings"
else
    echo "removed Mace; accounts and settings are kept in $data/mace and $config/mace"
fi
