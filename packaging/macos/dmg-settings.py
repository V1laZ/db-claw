# dmgbuild settings for the DB Claw installer disk image.
#   dmgbuild -s packaging/macos/dmg-settings.py -D app="path/DB Claw.app" -D root=<repo> "DB Claw" out.dmg
# Layout matches dmg-background.html (660x400 points).
import os.path

application = defines["app"]  # noqa: F821 (provided by dmgbuild)
root = defines["root"]  # noqa: F821
here = os.path.join(root, "packaging", "macos")
app_name = os.path.basename(application)

format = "UDZO"
compression_level = 9
filesystem = "HFS+"
size = None

files = [application]
symlinks = {"Applications": "/Applications"}
icon = os.path.join(root, "assets", "icon.icns")  # volume icon

background = os.path.join(here, "dmg-background.tiff")
window_rect = ((200, 160), (660, 400))
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
include_icon_view_settings = True

icon_size = 128
text_size = 13
arrange_by = None
label_pos = "bottom"
icon_locations = {
    app_name: (170, 168),
    "Applications": (490, 168),
}
hide_extensions = [app_name]
