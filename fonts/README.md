# Fonts

Font files (`.ttf`, `.otf`) placed here are copied by `scripts/install.ps1` to
`%LOCALAPPDATA%\wdock\fonts`. When you click **Use font in Windows** in wdock's
settings and the chosen font is missing, `scripts/windows-font.ps1` installs
them for every user and then makes Windows use that font for its interface.

SF Pro is licensed by Apple and may not be redistributed, so it is not shipped
here. Download it from https://developer.apple.com/fonts/, extract the `.otf`
files from the installer and copy them into this folder (or straight into
`%LOCALAPPDATA%\wdock\fonts`). Font files in this folder are ignored by git.
