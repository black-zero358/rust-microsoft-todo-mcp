# Third-party notices

The project code is licensed under AGPL-3.0-only. This license choice does not change the licenses of third-party dependencies or upstream reference projects.

## Rust dependencies

The exact dependency versions and source checksums are recorded in `Cargo.lock`. Each dependency retains the license, copyright notices and attribution supplied in its published crate. Dependency license expressions and source locations can be inspected with:

```sh
cargo metadata --locked --format-version 1
```

The Docker build collects the license, copyright and notice files shipped in downloaded crate sources and the Rust toolchain into `/usr/share/licenses/rust-microsoft-todo-mcp/third-party/`. When distributing binaries separately, retain these applicable upstream files alongside the project license. The Rust standard library, bundled SQLite and container operating-system components also retain their upstream terms. Operating-system package notices are supplied by the base image under `/usr/share/doc`.

## Reference project

[rust-email-mcp](https://github.com/black-zero358/rust-email-mcp/tree/1f1a0b6d52ea2a92b8007758b5b55cb306766849) was consulted for architecture and development lessons. That project is MIT licensed. Its license remains unchanged by this project's adoption of AGPL-3.0-only. The inspected snapshot and lessons are documented in [architecture](docs/architecture.md#lessons-from-rust-email-mcp).

## License text

`LICENSE` reproduces the GNU Affero General Public License version 3 text published by the Free Software Foundation, obtained from the SPDX license-list-data [AGPL-3.0-only canonical text](https://github.com/spdx/license-list-data/blob/main/text/AGPL-3.0-only.txt). The GNU source is <https://www.gnu.org/licenses/agpl-3.0.txt>. Its standard appendix describes how authors can apply the license; this project's operative version choice is **AGPL-3.0-only**, as stated in `Cargo.toml` and `README.md`.
