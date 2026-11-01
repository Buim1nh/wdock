# Top bar alignment

When changing or adding top-bar controls, keep their visual and interactive
bounds vertically centred within the visible bar background (excluding its
bottom separator), with equal top and bottom margins. Use
`topbar::vertical_bounds` for the shared control row instead of introducing
independent vertical offsets. Keep tray icon and text centres aligned to that
row, and centre the compact Dynamic Island within the configured top-bar
height. Check fractional DPI and odd pixel heights; a one-pixel difference can
be unavoidable when integer window coordinates are required.
