# WIP state: feat/rulers-like-word
Intern: Sol (sol high), stopped minutes after starting. Not built, not reviewed.
Scope: F1-F9, G1, G2, G4, G6, G7 (ruler band 17 pt, inset 1/1; number token ruler_number #272727 belongs HERE; numbers 3 pt above centre; ticks anchored at bottom ~9/~4 pt; tab selector glyph without box; hourglass ~10x16 stroke ~#7A7A7A; right-margin marker own rounded shape 10x8.5 anchored at the base; vertical ruler 16 pt reserve with rotated numbers #1A1A1A; top gap 11.5; layered page shadow ~9 pt; canvas colour stays (theme branch)).
Done so far:  4 files changed, 120 insertions(+), 45 deletions(-) - read the diff; nothing confirmed.
Left: everything above, tests, docs, fmt/clippy/test, review.
Briefings used: ~/wordcraft-wt/_briefings/comum.md + <branch>.md on the Mac (audit: docs/audit/parity-ui-2026-10-16.md on audit/parity-ui). Base: storytold main eae2421. Rules: no unwrap/expect/panic/unsafe; Mac-only via cfg!(target_os = "macos") in normal ifs; one ui-parity.md row + ROADMAP log row; tests for every measured size; existing theme token values unchanged (theme branch owns them).
