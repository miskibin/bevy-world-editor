# Viewport controls + edit-mode defaults (2026-07-23)

Distilled from how real 3D/level/terrain editors handle the viewport (Unreal Editor,
Unity Scene view, Blender, World Machine/Gaea). Drives the flycam + default-post changes.

## What the incumbents do

- **Navigation.** RMB-hold "fly" (WASD + QE, scroll = speed) is the FPS-style scheme
  (Unreal, Unity, godot). MMB-drag = screen-space **pan** is universal. **F = frame/focus**
  the selection/cursor is near-universal. Orbit (Alt/MMB) and zoom-to-cursor exist too but
  are secondary to the fly scheme we already ship.
- **Camera speed.** Per-scroll *multiplicative* speed (Unreal's 1-8 scale, Unity's scroll
  accel), and the current speed is surfaced (Unreal shows a speed readout / setting). Shift =
  boost is standard.
- **Mouse look.** NO smoothing/acceleration on look by default — deltas map straight to yaw/
  pitch; mouse deltas are per-frame and must **not** be dt-scaled (dt-scaling makes look
  laggy + framerate-dependent — a classic bug). Sensitivity is a setting; invert-Y is a
  setting.
- **Edit-mode rendering.** Editors default to a fast, mostly-unlit-ish viewport; heavy
  cosmetic effects (AO, bloom, DoF, volumetrics/haze, god rays, supersampling) are **opt-in**.
  The edit viewport prioritises responsiveness + framerate over beauty.

## Decisions applied here

- Keep the existing RMB-fly + WASD/QE + scroll-speed + Shift-boost scheme.
- Add **MMB-drag pan** (screen-space, speed scales with fly speed) and **F = frame**
  (snap onto the terrain under the cursor, else map centre).
- Look stays un-smoothed and NOT dt-scaled (already correct); movement stays dt-scaled.
- Expose **mouse sensitivity**, **camera speed**, **invert-Y** in a right-panel "Camera"
  section; show live camera speed in the status bar.
- **Post OFF by default** in the interactive editor: SSAO, bloom, god rays, DoF, height-fog/
  haze all off; SSAA = 1.0. Basic tonemapping + exposure stay (sane image). Every effect is
  still one Graphics-panel toggle away. The visual harnesses (`WED_SHOT/CLIP/PROFILE/
  EDITDEMO`) keep the full cinematic pipeline via the `genrun::cinematic_defaults()` guard so
  their output stays comparable. Aligns with the existing LOW-GPU master switch (a stronger
  version of the same idea).
- Perf: the atmospherics/god-rays/DoF fullscreen passes now early-out when gated off, so a
  disabled effect costs nothing per frame (not just an invisible pass that still runs).
