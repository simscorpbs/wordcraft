# WIP state: feat/status-bar-like-word
Intern: Luna (luna max), stopped mid-task. Not built, not reviewed.
Scope: H1 (Focus drawn over the language: reserve the right block width, hide/truncate language), then H2-H8 (height 28, side margins 24, view buttons Print/Web/Outline/Draft 32x22 with ~16 pt glyphs, checked bg #A9A9A9 + icon #333333 in NEW tokens, zoom slider 100 pt with ~18 pt knob, text ~12 pt #4E4E4E, top border 1 pt #B4B4B4). H9 out.
Done so far (uncommitted diff at pause):  4 files changed, 206 insertions(+), 44 deletions(-) - read the diff; nothing confirmed.
Left: finish H1-H8, tests per size, docs rows, fmt/clippy/test, review.
Briefings used: ~/wordcraft-wt/_briefings/comum.md + <branch>.md on the Mac (audit: docs/audit/parity-ui-2026-10-16.md on audit/parity-ui). Base: storytold main eae2421. Rules: no unwrap/expect/panic/unsafe; Mac-only via cfg!(target_os = "macos") in normal ifs; one ui-parity.md row + ROADMAP log row; tests for every measured size; existing theme token values unchanged (theme branch owns them).
