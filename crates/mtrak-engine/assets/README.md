# Embedded Fennel compiler

`fennel.lua` is the official standalone Fennel compiler, checked into source
control and embedded into the mtrak executable with `include_str!`. Normal
builds never download anything and never require a Fennel executable.

- **Compiler:** Fennel 1.6.1
- **Source:** <https://fennel-lang.org/downloads/fennel-1.6.1.lua>
- **License:** MIT (the asset carries its own SPDX header)

Update it with the repository's Makefile target, which downloads the official
release and only replaces the asset after a successful download:

```sh
make update-fennel FENNEL_VERSION=1.6.1
```

The reported compiler version is asserted against `FENNEL_VERSION` in
`crates/mtrak-engine/src/fennel.rs`, so bump the constant in the same change
when the asset is replaced.
