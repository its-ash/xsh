# SwiftTerm, vendored

Upstream: https://github.com/migueldeicaza/SwiftTerm, tag `v1.99.2`
(commit `a183c15b52858530a17de57dcc72b93eeebc9857`). License: `LICENSE` (MIT).

This copy holds only what the macOS library needs: `Sources/SwiftTerm`, the
build-info generator and plugin, `swifterm-terminfo` and `LICENSE`.
`Package.swift` is trimmed to those targets.

Every change is marked `xsh vendor patch` in the source.

## 1. Wait for a drawable without the render-state lock

Files: `Sources/SwiftTerm/Apple/TerminalRenderOwner.swift` (`renderPublishedFrame`),
`Sources/SwiftTerm/Apple/Metal/MetalTerminalRenderer.swift` (`drawableSurface`,
`noteDrawableUnavailable`).

Symptom: in full screen, the whole xsh app froze for 50 to 600 ms on key
presses.

Cause: the render thread called `renderMetal()`, which holds the render-state
lock through `MetalTerminalRenderer.render`, and that calls
`CAMetalLayer.nextDrawable()`. In full screen the compositor sometimes keeps
the drawables, and `nextDrawable()` then blocks for up to a second. Each key
press takes the same lock on the main thread
(`TerminalView.configureInputSender` calls
`resetMetalCursorBlinkAfterInput()`), so the main thread waited for the
compositor.

Fix: the render loop acquires the drawable first, with no lock held, and then
passes it to `renderMetal(frame:)`, which `render` already accepts. If no
drawable comes, it does what `render` did in that case: drop the prepared
snapshot and mark a pending redraw.

The same code is unchanged in `v1.99.3`. Drop this patch once upstream fixes
it.

This keeps the app responsive, but it cannot make macOS show the frames
sooner. Measured on an M1 Pro with macOS 27.2.1, a Metal frame in a
full-screen window reached the screen 380 to 470 ms after the key press at
the median (up to 1.2 s), against about 32 ms in a normal window. Neither
surface (`CAMetalLayer` or `MTKView`), display sync, frame-rate hints,
`present(afterMinimumDuration:)` nor a non-opaque layer changed that, and the
CPU renderer was late there too. A normal window of the same size was not. So
xsh does not use the native full screen: see `TerminalWindow.toggleScreenFill()`
in `app/Sources/XshApp/TerminalWindow.swift`.

## 2. Fixed build information

File: `Sources/SwiftTermBuildInfoGenerator/BuildInfoGenerator.swift`.

Upstream reads the branch, tag and commit with Git. Inside the xsh repository
that would report xsh's own commit in the terminal's XTVERSION reply. The
generator now reports the upstream tag and commit above, marked as modified.

## Upgrading

1. Fetch the new upstream tag and replace `Sources/` and `Plugins/`.
2. Apply the patches above again, unless upstream has fixed them.
3. Compare the upstream `Package.swift` with the trimmed one here.
4. Update the tag and commit here and in `BuildInfoGenerator.swift`.
