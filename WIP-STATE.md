# WIP state: feat/mac-title-bar-like-word
Intern: Luna (luna max), stopped late in the task. Not built, not reviewed.
Scope: B1 (document title with ellipsis), B2 (search as a magnifier only on macOS), B5 (traffic-light reserve 14/23/+3), B8 (focus background - overlaps the theme branch's inactive tokens; drop the colour part here at review), J2: ui.* options 'Show Discord button in the title bar' and 'Show account picture in the title bar' in Options > General, off on macOS, on elsewhere. Skip B4, B6, B7.
Done so far:  28 files changed, 394 insertions(+), 90 deletions(-) (chrome.rs, lib.rs ui.* commands, options.rs, theme.rs, i18n rows in 22 languages, docs) - read the diff; nothing confirmed.
Left: check against scope, remove B8 colour duplication, tests, fmt/clippy/test, review.
Briefings used: ~/wordcraft-wt/_briefings/comum.md + <branch>.md on the Mac (audit: docs/audit/parity-ui-2026-10-16.md on audit/parity-ui). Base: storytold main eae2421. Rules: no unwrap/expect/panic/unsafe; Mac-only via cfg!(target_os = "macos") in normal ifs; one ui-parity.md row + ROADMAP log row; tests for every measured size; existing theme token values unchanged (theme branch owns them).
