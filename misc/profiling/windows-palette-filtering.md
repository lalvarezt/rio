# Command and font palette filtering

Static analysis at `1699eb81e9` found that every fuzzy comparison lowercased both
strings and collected both into character vectors. Even an empty query prepared
the entire target before returning score zero. Filtering runs for drawing,
hover hit testing, keyboard navigation and selection.

## Change

The matcher now returns immediately for an empty query and walks the lowercase
strings through character iterators. It stops after the final query character
matches, since later target characters cannot affect the score. Full-string
Unicode lowercasing remains in place, including contextual Greek sigma and
multi-character lowercase mappings.

The same private function serves command and font lists. No cache, retained
state, new interface or invalidation rules are added. Result sorting and all
callers are unchanged. The change removes five Rust lines.

## Native benchmark

An ignored Windows probe compiles the actual command palette module and calls
its filtering and selection methods. The baseline executable contains the
unchanged matcher. Its frozen implementation also serves as a differential
reference in both executables.

Three interleaved rounds alternate the before/after order. Each case contains
seven batches of 50 filter operations. Timing excludes setup and allocation
counting; a separate filter call measures allocation calls and requested bytes.
No compilation or screenshot capture runs during comparison. These are fast
debug builds, so the results describe this workload rather than release latency.

The installed-font dataset contains 375 family names from the Windows installed
font collection. The larger dataset derives 1,000 names from those families
with numeric suffixes. It is a scaling case, not a claim of 1,000 installed fonts.
The table reports medians of per-run median times for one complete filtering
operation, including stable result sorting.

| List and query | Time before / after, µs | Allocations before / after |
| --- | ---: | ---: |
| Commands, empty | 16.9 / 2.7 | 96 / 4 |
| 375 installed fonts, empty | 296.1 / 25.1 | 1,450 / 9 |
| 375 installed fonts, `mono` | 385.1 / 117.9 | 2,197 / 756 |
| 375 installed fonts, no match | 657.5 / 101.0 | 2,566 / 750 |
| 1,000 font names, empty | 739.8 / 50.0 | 4,007 / 10 |
| 1,000 font names, `mono` | 926.6 / 338.8 | 6,005 / 2,008 |

Empty installed-font filtering takes about 92% less time; the matching `mono`
query takes about 69% less. Empty matching itself allocates nothing, while
collecting and sorting results still allocates. Nonempty queries retain two
lowercase strings per comparison. These measurements establish lower filtering
cost, not a frame-rate or input-to-display latency improvement.

## Behavior validation

Each executable checks 6,987 query/target scores against the old matcher,
including mixed case, punctuation, combining characters, accented text,
Greek contextual case, Turkish dotted I, CJK and supplementary-plane characters.
Nine complete ranked lists retain the same scores, stable tie order, titles,
shortcuts and actions. Font selection, keyboard movement, theme-command gating
and returning from fonts to commands also pass. The native probe exits zero in
all six confirming runs.

The final Windows debug executable builds in 5.38 seconds. Native captures
exercise opening the palette, filtering and executing `List Fonts`, filtering
font families, moving selection and closing the palette. No repository tests
or runtime logging are added.

Raw samples are under ignored `target/profiles/palette-filter-compare` and
captures under `target/profiles/palette-capture-final`. The probe and build
helpers remain under ignored `target/rio-palette-*`. The Windows inspection
executable is `D:\git-repos\rio\target\debug\rio-render-inspect.exe`; `rio.exe`
receives the same executable.
