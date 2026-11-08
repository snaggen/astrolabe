name := 'astrolabe'
appid := 'io.github.snaggen.Astrolabe'

# Install destinations. Override `prefix` for a user install, e.g.
#   just prefix=~/.local install
rootdir := ''
prefix := '/usr'
base-dir := absolute_path(clean(rootdir / prefix))

bin-src := 'target/release' / name
bin-dst := base-dir / 'bin' / name

desktop := appid + '.desktop'
desktop-src := 'res' / desktop
desktop-dst := base-dir / 'share/applications' / desktop

icon := appid + '.png'
icon-src := 'res/icons/hicolor/512x512/apps' / icon
icon-dst := base-dir / 'share/icons/hicolor/512x512/apps' / icon

svg-icon := appid + '.svg'
svg-icon-src := 'res/icons/hicolor/scalable/apps' / svg-icon
svg-icon-dst := base-dir / 'share/icons/hicolor/scalable/apps' / svg-icon

# Build the release binary.
build-release:
    cargo build --release

# Install the binary, desktop entry, and icon under `prefix`.
install:
    install -Dm0755 {{bin-src}} {{bin-dst}}
    install -Dm0644 {{desktop-src}} {{desktop-dst}}
    install -Dm0644 {{icon-src}} {{icon-dst}}
    install -Dm0644 {{svg-icon-src}} {{svg-icon-dst}}

# Install only the desktop entry and icon (enough for the window icon to show
# when running via `cargo run`, since the compositor matches the app id).
install-resources:
    install -Dm0644 {{desktop-src}} {{desktop-dst}}
    install -Dm0644 {{icon-src}} {{icon-dst}}
    install -Dm0644 {{svg-icon-src}} {{svg-icon-dst}}

# Remove all installed files under `prefix`.
uninstall:
    rm -f {{bin-dst}} {{desktop-dst}} {{icon-dst}} {{svg-icon-dst}}
