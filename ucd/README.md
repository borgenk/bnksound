# Vendored Unicode Character Database

The UCD files the grapheme property tables are generated from, pinned to one
Unicode version and committed so a build never touches the network. Everything
here is verbatim from the Unicode Consortium's public files.

**Version: 18.0.0**

| File | Feeds | Property used |
| --- | --- | --- |
| `GraphemeBreakProperty.txt` | `src/render/grapheme_tables.rs` | Grapheme_Cluster_Break |
| `emoji-data.txt` | `src/render/grapheme_tables.rs` | Extended_Pictographic (rule GB11) |
| `DerivedCoreProperties.txt` | `src/render/grapheme_tables.rs` | Indic_Conjunct_Break (rule GB9c) |
| `GraphemeBreakTest.txt` | `grapheme.rs` test | the official UAX #29 conformance suite |

`grapheme_tables.rs` is committed pre-generated, so a build reads none of these;
only the conformance suite is opened, and only by a test. Regeneration is a pure
offline function of this directory and reproduces the committed file byte for
byte:

```sh
cargo run --example gen_tables
```

## Refreshing on a Unicode bump

These came from the Arch `unicode-character-database` package
(`/usr/share/unicode/`). To move to a new version, copy the four files above
from there (note the `extracted/`, `auxiliary/`, and `emoji/` subpaths) or fetch
them from `https://www.unicode.org/Public/<version>/ucd/`, run the generator,
and re-run the conformance test.
