# tawaki

The package manager for [Fiordland Linux](https://fiordland.apollo44.lat). One command that installs from the cleanest source: pacman, then Flathub, then the AUR. All sources are looked up in parallel.

Works on any Arch-based system.

```
tawaki install spotify      i
tawaki remove spotify       r, rm
tawaki update               u, up
tawaki search obs           s
tawaki list                 ls
tawaki install foo --from aur
```

If the top pick is a Flatpak that would pull a big runtime you don't have yet, tawaki takes a prebuilt pacman or AUR package instead and tells you how much it skipped. The Flathub catalogue refreshes in the background (`dist/tawaki-catalogue.timer`), so searches never wait on it.

## Config

`~/.config/tawaki/config.toml`

```toml
priority = ["pacman", "flatpak", "aur"]
aur_helper = "auto"
flatpak_scope = "user"
flatpak_remote = "flathub"
prefer_light = true
```

## Build

```
cargo build --release --locked
```

Optional: `flatpak` for Flathub apps, `paru` or `yay` for AUR installs (falls back to makepkg), `pacman-contrib` for update counts.

## License

GPL-3.0. See [LICENSE](LICENSE).
