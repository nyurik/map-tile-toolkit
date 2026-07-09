# rust-crate-template

[![GitHub repo](https://img.shields.io/badge/github-nyurik/rust--crate--template-8da0cb?logo=github)](https://github.com/nyurik/rust-crate-template)
[![crates.io version](https://img.shields.io/crates/v/rust-crate-template)](https://crates.io/crates/rust-crate-template)
[![crate usage](https://img.shields.io/crates/d/rust-crate-template)](https://crates.io/crates/rust-crate-template)
[![docs.rs status](https://img.shields.io/docsrs/rust-crate-template)](https://docs.rs/rust-crate-template)
[![crates.io license](https://img.shields.io/crates/l/rust-crate-template)](https://github.com/nyurik/rust-crate-template/blob/main/LICENSE-APACHE)
[![CI build status](https://github.com/nyurik/rust-crate-template/actions/workflows/ci.yml/badge.svg)](https://github.com/nyurik/rust-crate-template/actions)
[![Codecov](https://img.shields.io/codecov/c/github/nyurik/rust-crate-template)](https://app.codecov.io/gh/nyurik/rust-crate-template)

A template repository for new Rust library crates. It bundles the tooling and
CI shared across these crates so a new library starts with everything wired up.

## Using this template

* Create a new repository from this template (GitHub's *Use this template*
  button, or copy the files).
* Search/Replace all cases of `rust-crate-template`, `rust--crate--template`, and `rust_crate_template`.
* Update the `description`, `keywords`, `categories`, and `rust-version` in [`Cargo.toml`](Cargo.toml).
* In GitHub project settings, modify:
  * `General Page`
     * `Enable release immutability Loading` - set
     * `Wiki` - unset (rarely useful)
     * `Projects` - unset (rarely useful)
     * `Pull Requests`
       * disallow `merge` and `rebase` - keeping history cleaner
       * `squash` - default message to `Pull request title and description`
     * `Always suggest updating pull request branches` - set
     * `Allow auto-merge` - set
     * `Automatically delete head branches` - set
* Update this file

For a **binary** crate instead of a library, rename `src/lib.rs` to
`src/main.rs` (or add one alongside), and remove it from `.gitignore`

### What's included

* **CI** ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)) — build,
  test, MSRV check with minimal dependency versions, code coverage, and
  automated releases via [release-plz](https://release-plz.dev).
* **[`justfile`](justfile)** — common developer tasks (`test`, `clippy`,
  `fmt`, `coverage`, `msrv`, `semver`, …).
* **Lints** — shared `clippy` and `rustc` lint configuration in
  [`Cargo.toml`](Cargo.toml) and [`clippy.toml`](clippy.toml).
* **Formatting** — [`.editorconfig`](.editorconfig),
  [`tomlfmt.toml`](tomlfmt.toml), and a
  [`.pre-commit-config.yaml`](.pre-commit-config.yaml).
* **Dependabot** — grouped dependency and GitHub Actions updates with
  auto-merge for patch releases.
* **Dual licensing** — MIT and Apache-2.0.

## Development

* This project is easier to develop with [just](https://github.com/casey/just#readme), a modern alternative to `make`.
  Install it with `cargo install just`.
* To get a list of available commands, run `just`.
* To run tests, use `just test`.

## License

Licensed under either of

* Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)
  at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the
Apache-2.0 license, shall be dual-licensed as above, without any
additional terms or conditions.
