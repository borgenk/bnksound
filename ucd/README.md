# Vendored Unicode Character Database

The UCD files the grapheme property tables are generated from, pinned to one
Unicode version and committed so a build never touches the network.

**Version: 18.0.0**

| File | Feeds | Property used |
| --- | --- | --- |
| `GraphemeBreakProperty.txt` | `src/platform/grapheme_tables.rs` | Grapheme_Cluster_Break |
| `emoji-data.txt` | `src/platform/grapheme_tables.rs` | Extended_Pictographic (rule GB11) |
| `DerivedCoreProperties.txt` | `src/platform/grapheme_tables.rs` | Indic_Conjunct_Break (rule GB9c) |
| `GraphemeBreakTest.txt` | `grapheme.rs` test | the official UAX #29 conformance suite |

`grapheme_tables.rs` is committed pre-generated, so a build reads none of these;
only the conformance suite is opened, and only by a test. Regeneration is a pure
offline function of this directory and reproduces the committed file byte for
byte:

```sh
make tables
```

## What is committed

Three of the four files are verbatim upstream. `DerivedCoreProperties.txt` is
not: upstream it carries every derived property, around 13,900 lines, of which
the only one read here is Indic_Conjunct_Break. The committed copy keeps the
file header and that one section, which is this filter:

```sh
awk 'NR<=9{print;next} /^# Derived Property: Indic_Conjunct_Break/{f=1} f'
```

To check the trimmed copy against upstream, run the same filter on the original
and compare:

```sh
curl -s https://www.unicode.org/Public/18.0.0/ucd/DerivedCoreProperties.txt \
  | awk 'NR<=9{print;next} /^# Derived Property: Indic_Conjunct_Break/{f=1} f' \
  | diff - DerivedCoreProperties.txt
```

## Refreshing on a Unicode bump

These came from the Arch `unicode-character-database` package
(`/usr/share/unicode/`). To move to a new version, copy the four files above
from there (note the `extracted/`, `auxiliary/`, and `emoji/` subpaths) or fetch
them from `https://www.unicode.org/Public/<version>/ucd/`, put
`DerivedCoreProperties.txt` through the filter above, run the generator, and
re-run the conformance test. The generator rewrites `grapheme_tables.rs` in
place, so the diff it leaves is the review.
