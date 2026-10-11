# Validation for Gradient Editor follow-up PR #3345

Date: 2026-10-11. Final source: `ce8915f2ac0d425a93ce7ef9b42c28cfd77f33b1`, including main `67e3d9be`. Windows MSVC Rust/Cargo 1.99, native wgpu with the DX12 Microsoft Basic Render Driver. The parent Properties contribution remains in this draft until #2859 merges.

## Final source checks

| Command/check | Result |
|---|---|
| `cargo test --locked -p photocraft-engine --no-fail-fast` | 1,342 passed, 17 ignored, no failures; exit 0 |
| `cargo test --locked -p photocraft-ui-egui --lib` | 1,880 passed, 10 ignored, no failures; exit 0 |
| `cargo test --locked -p photocraft-ui-egui --test checker_canvas --test drag_preview_canvas --no-fail-fast -- --test-threads=1` | Checker: 6 passed; drag previews: 3 passed; exit 0 |
| `cargo test --locked -p photocraft-engine --test panic_hunt -- --ignored` | 1 passed; exit 0 |
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --locked -p photocraft-engine -p photocraft-ui-egui --all-targets -- -D warnings` | Passed |
| `cargo xtask layers` | Passed |
| `cargo xtask wasm` | Passed |
| `cargo xtask i18n-coverage` | Existing extracted keys: 1,643/1,643 in all languages; dynamic preference caption separately present once in all 16 catalogs |
| `cargo xtask scorecard --check` | Passed; generated audit records 37 unread of 153 preference fields |
| Standard Windows helper, Debug BuildOnly | Passed; final app build completed without launching or terminating a GUI instance |
| Full PR diff and whitespace review | Passed; clean source worktree; only source changes and generated scorecard in the PR |

## Full UI run and native failures

A full `cargo test --locked -p photocraft-ui-egui --no-fail-fast` on the first merged revision `220a32ee` recorded 1,958 passed tests and 10 ignored across 22 completed targets. Two additional test binaries, `checker_canvas` and `drag_preview_canvas`, exited with `0xc0000005 (STATUS_ACCESS_VIOLATION)`, so the full command exited 101. These were native process crashes, not successful tests or assertion-only failures.

Both affected binaries subsequently completed on final source with `--test-threads=1`: all nine tests passed. The previously observed marquee contrast assertion also passed in that retry. Root cause of the original native exits has not been conclusively diagnosed. No canvas implementation or assertion threshold was changed in this contribution. Retry and final-library counts are separate evidence and must not be added to the original full-run count.

The complete UI integration suite was not repeated after the final main refresh, which brought font-preview and mask-application changes. Final full engine and UI library runs, affected canvas binaries, checks and the app build were repeated. Browser runtime, Linux/macOS execution, release web size gate and full-workspace/corpus checks were not run locally. GitHub CI covers the remote gates; no overall green CI status is claimed.

## Visual evidence and CI

Native snapshot build and refreshed English after captures use `220a32ee`, before the final unrelated font/mask main refresh. All displayed gradient controls are unchanged by that refresh. Captures cover grouped editor presets, Live/Classic dropdowns, New/inline naming, Properties layer targeting, both icon variants, the saved Interface checkbox and 200% DPI. Test data is synthetic and settings are isolated.

CI and Documentation workflows for final head report `action_required`, awaiting approval from an upstream maintainer. The submitting account has no upstream write permission. [CI run](https://github.com/storytold/photocraft/actions/runs/38126584386); [Documentation run](https://github.com/storytold/photocraft/actions/runs/38126584558). No workflow settings were changed.