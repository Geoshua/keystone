# Keystone

Software keystone correction for projectors that only have vertical keystone (or none).
It captures any window (a game, YouTube in a browser, a video player), warps it with a
perspective transform and shows the result in its own window, which you put fullscreen
on the projector.

Windows 10 1903+ / Windows 11. Written in Rust (eframe/egui + wgpu, Windows Graphics Capture).

## Use

1. Run `keystone.exe` and drag its window onto the projector.
2. Press **F11** for fullscreen.
3. Leave **Calibration pattern** selected and adjust until the circles on the wall are round and the border is rectangular:
   - **Keystone sliders**, like a projector menu. Horizontal + narrows the right edge, Vertical + narrows the top edge. With no corner selected, the arrow keys do the same (Shift = 5 steps).
   - **Four corners**: drag the yellow handles, or click a corner and nudge it with the arrow keys (Shift = 10 px).
4. Pick the source window from the **Source** list. Press **Tab** to hide the controls and the mouse cursor.

The geometry belongs to the projector's placement, not the content, so one setting is saved and reused for every source.

### Tips

- **Browser video**: put the video in fullscreen on your main monitor (YouTube `f`, or F11). The capture is then just the picture. Otherwise tick **Crop title bar** to remove the window chrome.
- **Games**: use borderless-windowed mode. Exclusive fullscreen can't be captured.
- **Resolution**: with **Match window to source resolution** on, the output window takes the source's pixel size, so the frame is warped at native resolution. In fullscreen the output takes the projector's resolution. The panel shows both sizes and warns you when they differ.
- **DRM**: Netflix, Prime and Disney+ in Chrome or Edge may capture as black, because hardware DRM blocks screen capture. YouTube works.

## Keys

| Key | Action |
|---|---|
| Tab | Show or hide the controls (hidden = clean output with no cursor) |
| F11 / Esc | Toggle fullscreen / leave it (Esc first deselects a corner) |
| ← → | Horizontal keystone, or nudge the selected corner |
| ↑ ↓ | Vertical keystone, or nudge the selected corner |
| G | Show the alignment grid over the source |

## Build

On Windows:

```
cargo build --release
```

Cross-compile from Linux:

```
rustup target add x86_64-pc-windows-gnu
sudo apt install mingw-w64
cargo build --release --target x86_64-pc-windows-gnu
```

`cargo test` runs the homography and keystone geometry tests on any OS. On Linux and macOS the app runs with the calibration pattern only, which is useful for UI work.

## How it works

- `src/source.rs` captures a window through Windows Graphics Capture (`windows-capture` crate) as BGRA frames. These are copied into a shared buffer, and the UI is asked to repaint.
- `src/warp.rs` builds the quad from the H/V keystone and the corner offsets. It computes the unit-square→quad homography (Heckbert) and inverts it.
- `src/warp.wgsl` maps each output pixel back through the inverse homography into the source texture. It takes 4 rotated-grid samples per pixel so the narrowed side and the edges don't alias.
- `src/render.rs` uploads the frame to a wgpu texture and draws it inside egui's render pass.

## Known limits (v0.1)

- Frames take a GPU→CPU→GPU round trip, which costs about one frame of latency. A later version can share the D3D11 texture with wgpu directly.
- Mouse input on the warped output doesn't reach the source window. Control the source on your main screen.
