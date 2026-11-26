# tawaki

<p><a href="https://donate.stripe.com/8x25kFfe96fn0WL6X12wU00"><img src=".github/donate.svg" alt="Donate to Fiordland"></a> <a href="https://discord.gg/gy4ANH2Q9V"><img src=".github/discord.svg" alt="Join the Fiordland Discord"></a></p>

Created by [Apollo](https://apollo44.lat) ([3cfl](https://github.com/3cfl) on GitHub, u5sk on Discord), as part of [Fiordland Linux](https://github.com/Fiordland-Linux/fiordland).

The package manager for [Fiordland Linux](https://fiordlandlinux.org). One command that installs from the cleanest source: pacman, then Flathub, then the AUR. All sources are looked up in parallel.

Works on any Arch-based system.

```
tawaki install spotify      i
tawaki remove spotify       r, rm
tawaki update               u, up
tawaki search obs           s
tawaki list                 ls
tawaki install foo --from aur
tawaki search obs --json      machine-readable output (also list, check)
tawaki completions bash       shell completions (bash, zsh, fish, elvish, powershell)
```

AUR answers are cached for a couple of minutes, and every package name goes to the AUR in one batched request.

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
