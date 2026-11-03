# FilmCraft, Filmsquare build

This is a modified version of FilmCraft, kept by The Dream Company as the editor inside
Filmsquare. It is based on
[FilmCraft](https://github.com/storytold/filmcraft) by the ArtCraft team. It is not made,
sponsored or endorsed by them, and it is not an ArtCraft product.

The `filmsquare` branch is one small patch on an upstream tag, so it can be rebased on newer tags.

## What changed

- The ArtCraft name, wordmark, logos and links are gone (`docs/brand/` keeps only the licence that
  requires this). This file replaces the upstream README, which showed them.
- For use inside a host product: no Discord button, no Home or Import entry in the header, no
  update check, no community, help, GitHub or contributor-profile links. Nothing else was restyled.
- `NOTICE` and `ATTRIBUTION.md` say this is a modified version; release packages include
  `ATTRIBUTION.md`.
- `.github/workflows/filmsquare-release.yml` builds and publishes on a tag `fs-*`.
- symphonia (MPL-2.0) is used unmodified from crates.io, as listed in `Cargo.lock`.

`git diff <upstream tag> filmsquare` shows every change.

## Build

`cargo xtask web` builds the web site, see [docs/web.md](docs/web.md).
`cargo build --release --locked -p filmcraft-cli` builds the CLI.

## License

MIT OR Apache-2.0, at your option ([LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE)).
Copyright (c) 2026 ArtCraft Team and the FilmCraft contributors. Required notices are in
[NOTICE](NOTICE), third-party material in [ATTRIBUTION.md](ATTRIBUTION.md).
