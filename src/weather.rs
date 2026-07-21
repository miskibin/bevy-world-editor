//! Weather state — the single source of truth for rain/snow, and the smoothed light/fog
//! dimming that overcast skies imply. This module owns STATE ONLY: it never touches lights,
//! fog, or materials itself (that would fight `daycycle.rs`, which is the one place the sun /
//! ambient / fog are actually written). The contract is:
//!
//!   - `weather.rs` (here) holds `Weather` (mode + intensity) and eases a `WeatherDim`
//!     resource — three multipliers (sun, ambient, fog visibility) — toward the mood the
//!     current weather implies. Pure state; deterministic; no rendering.
//!   - `daycycle.rs` reads `WeatherDim` and COMPOSES it into the sun illuminance, ambient
//!     brightness, and fog visibility distance it already computes from the clock (weather
//!     multiplies the time-of-day values, so a rainy noon is just a dimmer noon).
//!   - `particles.rs` reads `Weather` (mode + intensity) to spawn/scale the rain or snow
//!     emitters.
//!
//! The dim is eased over ~3 s of real time rather than snapped so a weather change reads as
//! clouds rolling in, not a light switch — matching the "alive foundation" feel of the day
//! cycle. Wiring the two reader modules to consume these resources is done by the caller.

use bevy::prelude::*;

/// What's falling from the sky.
#[derive(Clone, Copy, PartialEq)]
pub enum WeatherMode {
    Clear,
    Rain,
    Snow,
}

/// The live weather. `intensity ∈ [0,1]` scales how much a given mode dims the world and how
/// thick its particle field is.
#[derive(Resource)]
pub struct Weather {
    pub mode: WeatherMode,
    pub intensity: f32,
}

impl Default for Weather {
    fn default() -> Self {
        // `WED_WEATHER=rain` or `WED_WEATHER=snow,0.9` stages weather for a screenshot (same
        // pattern as `WED_TIME` in daycycle.rs). Anything unparseable falls back silently to
        // a clear sky — a bad env value must never crash the harness.
        let (mode, intensity) = std::env::var("WED_WEATHER")
            .ok()
            .map(|s| {
                let mut parts = s.split(',');
                let mode = match parts.next().map(|m| m.trim().to_ascii_lowercase()).as_deref() {
                    Some("rain") => WeatherMode::Rain,
                    Some("snow") => WeatherMode::Snow,
                    _ => WeatherMode::Clear,
                };
                // Optional second field overrides intensity; ignore garbage, keep the default.
                let intensity = parts
                    .next()
                    .and_then(|v| v.trim().parse::<f32>().ok())
                    .map(|v| v.clamp(0.0, 1.0))
                    .unwrap_or(0.7);
                (mode, intensity)
            })
            .unwrap_or((WeatherMode::Clear, 0.7));
        Weather { mode, intensity }
    }
}

/// Smoothed multipliers the rest of the app composes into its own values. All 1.0 = clear
/// weather (a no-op factor), so a reader can multiply unconditionally.
#[derive(Resource)]
pub struct WeatherDim {
    /// Sun illuminance multiplier (overcast = darker).
    pub sun: f32,
    /// Ambient brightness multiplier (overcast still has soft fill, so it dims less than sun).
    pub ambient: f32,
    /// Fog VISIBILITY-distance multiplier — smaller pulls the fog in closer (thicker haze).
    pub fog_mul: f32,
}

impl Default for WeatherDim {
    fn default() -> Self {
        // Start neutral so the world isn't dimmed for a frame before the first ease step.
        WeatherDim { sun: 1.0, ambient: 1.0, fog_mul: 1.0 }
    }
}

/// The mood a weather state implies, before smoothing.
fn targets(w: &Weather) -> (f32, f32, f32) {
    let i = w.intensity.clamp(0.0, 1.0);
    match w.mode {
        WeatherMode::Clear => (1.0, 1.0, 1.0),
        // Rain is the darkest, thickest mood; ambient holds up more than the sun.
        WeatherMode::Rain => (lerp(1.0, 0.25, i), lerp(1.0, 0.55, i), lerp(1.0, 0.45, i)),
        // Snow scatters a lot of light back, so it stays brighter than rain at equal intensity.
        WeatherMode::Snow => (lerp(1.0, 0.45, i), lerp(1.0, 0.75, i), lerp(1.0, 0.55, i)),
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

pub struct WeatherPlugin;

impl Plugin for WeatherPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Weather>()
            .init_resource::<WeatherDim>()
            .add_systems(Update, drive_weather);
    }
}

/// Ease `WeatherDim` toward the current weather's targets. Frame-rate-independent exponential
/// smoothing (the `1 - e^(-dt/tau)` form) so the ~3 s settle is the same whether we run at 30
/// or 300 fps — never a per-frame constant lerp, which would drift with the frame rate.
fn drive_weather(time: Res<Time>, weather: Res<Weather>, mut dim: ResMut<WeatherDim>) {
    // tau ≈ 1 s → ~95% of the way there in 3 s (three time constants).
    const TAU: f32 = 1.0;
    let k = (1.0 - (-time.delta_secs() / TAU).exp()).clamp(0.0, 1.0);
    let (sun, ambient, fog_mul) = targets(&weather);
    dim.sun += (sun - dim.sun) * k;
    dim.ambient += (ambient - dim.ambient) * k;
    dim.fog_mul += (fog_mul - dim.fog_mul) * k;
}
