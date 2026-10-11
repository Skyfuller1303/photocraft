# Gradient Editor follow-up: native English screenshots

Source after: `220a32eeb5526316c64a44e392253cca4293b5bc` on `ui/gradient-editor-presets`, including main `e46f119e`. Before: unchanged parent PR #2859 (`4415a3dbc81a3dc7d72f774fd08c9d4086ca2f28`). Native offscreen wgpu captures, Pro medium theme, synthetic 640 × 480 document and English language with isolated test settings. No personal or proprietary reference images are included.

| Behavior | Crop | Full capture |
|---|---|---|
| Parent editor before grouped browser | [Before](before-editor-crop-en.png) | [Full before](before-editor-en.png) |
| Embedded grouped presets | [Editor](editor-purples-crop-en.png) | [Full editor](editor-purples-en.png) |
| Live toolbar dropdown | [Live](live-popup-crop-en.png) | [Full Live](live-popup-purples-en.png) |
| Classic toolbar dropdown | [Classic](classic-popup-crop-en.png) | [Full Classic](classic-popup-en.png) |
| New saves current gradient | [Save](popup-save-crop-en.png) | [Full save](popup-save-en.png) |
| Properties dropdown | [Properties](arrow-popup-crop-en.png) | [Full popup](arrow-popup-en.png) |
| New saves displayed Gradient Fill | [Layer save](arrow-save-crop-en.png) | [Full layer save](arrow-save-en.png) |
| Saved native icon setting, filtered by gradient | [Interface](icon-setting-prefs-crop-en.png), [checkbox](icon-setting-prefs-checkbox-en.png) | [Full Preferences](icon-setting-prefs-en.png) |
| Default thumbnails, Live Radial | [Toolbar](icon-setting-preview-crop-en.png), [Properties](icon-setting-preview-properties-en.png) | [Full previews](icon-setting-preview-en.png) |
| Native glyphs, Live Radial | [Toolbar](icon-setting-native-crop-en.png), [Properties](icon-setting-native-properties-en.png) | [Full native](icon-setting-native-en.png) |
| Native glyphs, Classic tool-only Radial; layer remains Linear | [Toolbar](icon-setting-classic-crop-en.png), [Properties](icon-setting-classic-properties-en.png) | [Full Classic native](icon-setting-classic-en.png) |
| Layer editor with native glyphs | [Editor](icon-setting-editor-crop-en.png) | [Full editor](icon-setting-editor-en.png) |
| Native glyphs at 200% DPI | [Toolbar](icon-setting-hidpi-crop-en.png), [Properties](icon-setting-hidpi-properties-en.png) | [Full HiDPI](icon-setting-hidpi-en.png) |

All after captures above were regenerated and visually inspected on the merged source. The Preferences capture uses the normal settings search to expose the option. Its true value was stored before opening the dialog; actual Apply/Cancel/restart behavior is tested separately. The popup's New action opens the existing inline name field. The exact originally reported opaque-preview gaps were not conclusively reproduced.

## Reproduce after captures

Build the native `snapshot` example at the source revision above. Fixtures contain control-protocol steps and synthetic data only. Use an isolated `PHOTOCRAFT_CONFIG_DIR` and run the example with `--size 1280x1200 --scale 1 --script` containing the JSON from the selected file in `fixtures/`. Use `--scale 2` for `icon-setting-hidpi.json`; control coordinates remain screen points. The before image comes from the parent binary.

This is a separate orphan asset branch. No screenshot binaries or local plan/log files are part of the source PR.