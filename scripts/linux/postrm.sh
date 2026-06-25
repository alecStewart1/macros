#!/bin/sh
# Rebuild desktop database and icon caches after our files are gone, so
# stale entries don't linger. Best-effort; never fail the uninstall.
set -e

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications || true
fi

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor || true
fi

exit 0
