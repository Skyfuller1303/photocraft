# Reuse pixel layer masks

Copy a mask independently of its layer with **Edit > Copy Mask**, or **Copy Mask** in the
Layers row's context menu. Select a destination layer and choose **Edit > Paste Mask** or
**Paste Mask** in that row's context menu. The mask clipboard is shared by all tabs in the
current PhotoCraft session and remains available after the source mask is edited or removed.

Dragging a pixel-mask thumbnail onto another layer moves the mask. Hold Alt (Option on macOS)
when releasing to copy it instead; the source keeps its mask. Dragging the layer name or its
content thumbnail still moves/copies the layer. Alt-click without dragging still views the mask.

If the destination already has a pixel mask, PhotoCraft asks **Replace Layer Mask?** before
changing anything. Cancel leaves both layers untouched. Each paste or move is one Undo step;
Undo restores the old destination mask and, for a move, the source mask. Locked layers refuse
mask changes, including moving a mask off a locked source.

The complete editable pixel mask is transferred: pixels, the sparse white/black default,
density, feather, enabled state and linked state. Copy and target edits remain independent.
Between differently sized documents, the source canvas is centered on the destination canvas
at the original pixel scale. Rendering clips to the destination canvas; the mask's sparse
default remains unchanged. No color conversion or mask-depth reduction is applied.

This clipboard is session-only and separate from the ordinary image and OS clipboards. It
does not transfer masks between application processes. Vector masks and automatic scaling
are outside this workflow. Copy/Paste Mask commands have no default shortcut; they can be
assigned through Keyboard Shortcuts.

## Engine commands

- `layer.layerMask.copy`: `{"layer": id}` snapshots a full pixel mask; omitted `layer` uses
  the active layer. Copy changes no document history.
- `layer.layerMask.paste`: `{"layer": id, "replace": false}` attaches the snapshot in one
  history step. An existing mask requires `replace: true`.
- `layer.layerMask.transfer`: `{"from": source_id, "layer": target_id, "copy": true,
  "replace": false}` copies or moves within the active document, preserving both clipboards.

All commands accept an optional `document` id to reject stale targets. UI replacement prompts
also check that the captured source, destination and clipboard have not changed. Headless
and automation callers must explicitly supply `replace: true`; they do not open a prompt.

Evidence: `mask_transfer_cmds/tests.rs` covers properties, independent edits, source removal,
8/16/32-bit mask samples, RGB/Gray/CMYK/Lab documents, dimensions, locks, invalid parameters
and Undo/Redo. `mask_transfer_ui/tests.rs` exercises real thumbnail drags, context menus,
replacement/cancellation and stale confirmations. These synthetic checks do not establish
complete Photoshop parity.
