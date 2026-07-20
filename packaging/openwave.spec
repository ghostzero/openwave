Name:           openwave
Version:        0.5.0
Release:        1%{?dist}
Summary:        Dual-mix virtual audio mixer for PipeWire
License:        MIT
URL:            https://github.com/ghostzero/openwave
Source0:        %{url}/archive/v%{version}/%{name}-%{version}.tar.gz
# Created with `cargo vendor` (see .copr/Makefile in the repository)
Source1:        %{name}-%{version}-vendor.tar.xz

BuildRequires:  cargo
BuildRequires:  rust >= 1.85
BuildRequires:  gcc
BuildRequires:  gtk4-devel >= 4.18
BuildRequires:  libadwaita-devel >= 1.8
BuildRequires:  pulseaudio-libs-devel
BuildRequires:  desktop-file-utils

# The PulseAudio compatibility layer of PipeWire is the API OpenWave talks to
Requires:       pipewire-pulse
# pw-cli / pw-link, used to wire and control effect chains
Recommends:     pipewire-utils
# LV2 effect browser (chains still load without it)
Recommends:     lilv
# VST2/VST3 effect hosting
Suggests:       Carla

%description
OpenWave is a dual-mix virtual audio mixer for Linux, inspired by Elgato
Wave Link. Every input channel has two independent faders: a Monitor Mix
(what you hear) and a Stream Mix, exposed as a virtual microphone
"Virtual Stream Mix" for OBS, Discord, or any other application. Channels
can capture microphones, application playback streams, or act as virtual
output devices, with optional per-channel LV2 and VST2/VST3 effect chains.

%prep
%autosetup
tar -xf %{SOURCE1}
mkdir -p .cargo
cat > .cargo/config.toml <<'EOF'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"

[net]
offline = true
EOF

%build
cargo build --release --locked

%install
install -Dm755 target/release/openwave %{buildroot}%{_bindir}/openwave
install -Dm644 data/de.ghostzero.OpenWave.desktop \
    %{buildroot}%{_datadir}/applications/de.ghostzero.OpenWave.desktop
install -Dm644 data/icons/hicolor/scalable/apps/de.ghostzero.OpenWave.svg \
    %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/de.ghostzero.OpenWave.svg

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/de.ghostzero.OpenWave.desktop

%files
%license LICENSE
%doc README.md
%{_bindir}/openwave
%{_datadir}/applications/de.ghostzero.OpenWave.desktop
%{_datadir}/icons/hicolor/scalable/apps/de.ghostzero.OpenWave.svg

%changelog
* Mon Jul 20 2026 René Preuß <hello@ghostzero.de> - 0.5.0-1
- Initial package
