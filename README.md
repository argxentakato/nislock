<div align="center">

# Nislock

</div>

Nislock is a screenlocker for Wayland compositors that implement the `ext-session-lock-v1` protocol, designed for the [Steam Deck](https://store.steampowered.com/steamdeck).

## Dependencies

- Rust toolchain supporting edition 2024 (e.g. stable via `rustup`)
- `pkg-config`
- `clang` / `libclang` (bindgen for `pam-sys`)
- `libxkbcommon`
- `libpam`

The Nix dev shell provides everything except the Rust toolchain:

```sh
nix develop
```

Without Nix, install the equivalent development packages for your distribution and set `LIBCLANG_PATH` if needed.

## Build

```sh
cargo build --release  # binary: target/release/nislock
```

Tests and lints:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Usage

Run `nislock` inside a Wayland session whose compositor supports `ext-session-lock-v1`. It reads the Steam Deck controller directly through `hidraw`, so the user needs read/write access to the `hid-steam` hidraw node.

| Variable | Purpose |
| --- | --- |
| `NISLOCK_USER` | User to authenticate (default: `$USER`, then `deck`) |
| `NISLOCK_PAM_SERVICE` | PAM service name (default: `login`) |

Sending `SIGUSR1` to the process unlocks and exits, like `swaylock`/`hyprlock`.

## License

© 2026 Unuaviro F. Argxentakato <unuaviro@argxentakato.me>

This program is free software: you can redistribute it and/or modify it under the terms of the GNU Affero General Public License as published by the Free Software Foundation, Version 3 of the License.

This program is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the GNU Affero General Public License for more details.

You should have received a copy of the GNU Affero General Public License along with this program. If not, see <https://www.gnu.org/licenses/>. 
