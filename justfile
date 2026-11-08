name := 'astrolabe'
appid := 'io.github.snaggen.Astrolabe'

# Install destinations. Override `prefix` for a user install, e.g.
#   just prefix=~/.local install
rootdir := ''
prefix := '/usr'
base-dir := absolute_path(clean(rootdir / prefix))

bin-src := 'target/release' / name
bin-dst := base-dir / 'bin' / name
# Where the binary ends up once installed (without `rootdir`, which is only a
# staging directory). The desktop entry runs this path, since launchers do not
# always have `prefix`/bin, such as ~/.local/bin, in their PATH.
bin-exec := absolute_path(clean(prefix)) / 'bin' / name

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
    sed 's|^Exec={{name}} |Exec={{bin-exec}} |' {{desktop-src}} | install -Dm0644 /dev/stdin {{desktop-dst}}
    install -Dm0644 {{icon-src}} {{icon-dst}}
    install -Dm0644 {{svg-icon-src}} {{svg-icon-dst}}

# Install only the desktop entry and icon (enough for the window icon to show
# when running via `cargo run`, since the compositor matches the app id).
install-resources:
    sed 's|^Exec={{name}} |Exec={{bin-exec}} |' {{desktop-src}} | install -Dm0644 /dev/stdin {{desktop-dst}}
    install -Dm0644 {{icon-src}} {{icon-dst}}
    install -Dm0644 {{svg-icon-src}} {{svg-icon-dst}}

# Remove all installed files under `prefix`.
uninstall:
    rm -f {{bin-dst}} {{desktop-dst}} {{icon-dst}} {{svg-icon-dst}}
