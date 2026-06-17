Name:           torrentngd
Version:        0.1.0
Release:        1%{?dist}
Summary:        TorrentNG native BitTorrent daemon

License:        GPL-3.0-or-later
URL:            https://github.com/snapetech/TorrentNG
Source0:        torrentngd-native-main-0-linux-x86_64.tar.gz
Source1:        torrentngd.service
Source2:        torrentngd.sysusers
Source3:        torrentngd.tmpfiles
Source4:        config.toml

BuildArch:      x86_64
BuildRequires:  systemd-rpm-macros
Requires(pre):  shadow-utils
Requires:       sqlite-libs
Requires:       systemd
%global debug_package %{nil}
%define __strip /bin/true
%{!?_unitdir:%global _unitdir /usr/lib/systemd/system}
%{!?_sysusersdir:%global _sysusersdir /usr/lib/sysusers.d}
%{!?_tmpfilesdir:%global _tmpfilesdir /usr/lib/tmpfiles.d}
%{!?systemd_post:%global systemd_post() %{nil}}
%{!?systemd_preun:%global systemd_preun() %{nil}}
%{!?systemd_postun_with_restart:%global systemd_postun_with_restart() %{nil}}
%{!?sysusers_create_compat:%global sysusers_create_compat() %{nil}}
%{!?tmpfiles_create:%global tmpfiles_create() %{nil}}

%description
TorrentNG native BitTorrent daemon with a bundled Web UI and systemd service.

%prep
%setup -q -c

%install
install -Dm755 usr/bin/torrentngd %{buildroot}%{_bindir}/torrentngd
install -dm755 %{buildroot}%{_datadir}/torrentng/webui
cp -a usr/share/torrentng/webui/. %{buildroot}%{_datadir}/torrentng/webui/
install -Dm644 %{SOURCE1} %{buildroot}%{_unitdir}/torrentngd.service
install -Dm644 %{SOURCE2} %{buildroot}%{_sysusersdir}/torrentngd.conf
install -Dm644 %{SOURCE3} %{buildroot}%{_tmpfilesdir}/torrentngd.conf
install -Dm644 %{SOURCE4} %{buildroot}%{_sysconfdir}/torrentngd/config.toml

%pre
%sysusers_create_compat %{SOURCE2}

%post
%systemd_post torrentngd.service
%tmpfiles_create %{_tmpfilesdir}/torrentngd.conf

%preun
%systemd_preun torrentngd.service

%postun
%systemd_postun_with_restart torrentngd.service

%files
%{_bindir}/torrentngd
%{_datadir}/torrentng/webui
%{_unitdir}/torrentngd.service
%{_sysusersdir}/torrentngd.conf
%{_tmpfilesdir}/torrentngd.conf
%config(noreplace) %{_sysconfdir}/torrentngd/config.toml
