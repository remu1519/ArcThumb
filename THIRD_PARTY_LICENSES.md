# Third-party licenses

ArcThumb itself is distributed under **MIT OR Apache-2.0** (see
`LICENSE-MIT` and `LICENSE-APACHE`). The following third-party
components are redistributed with `arcthumb-config.exe` and require
separate acknowledgement.

## Slint

[Slint](https://slint.dev/) is used as the GUI toolkit for
`arcthumb-config.exe` under the **Slint Royalty-Free License 2.0**.

Full license text:
https://github.com/slint-ui/slint/blob/master/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md

Attribution: ArcThumb satisfies the Slint Royalty-Free License 2.0
attribution requirement by displaying the `AboutSlint` widget inside
the **About** dialog of `arcthumb-config.exe` (reachable via the
**About** button in the settings window). The badge shows the Slint
logo and links back to https://slint.dev/.

Slint's own source is not modified and is linked statically into the
binary via the `slint` crate.

---

Other Rust crates used by ArcThumb (both the DLL and
`arcthumb-config.exe`) are redistributed under their respective MIT,
Apache-2.0, BSD, or similarly permissive licenses. Running
`cargo tree --format '{p} {l}'` from the repository root will list
every dependency together with its SPDX license identifier.
