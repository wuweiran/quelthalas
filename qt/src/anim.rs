//! A custom Windows Animation **v1** interpolator that reproduces Fluent's
//! cubic-bezier eased transition.
//!
//! The v2 transition library has `CreateCubicBezierLinearTransition`, but v1
//! (`IUIAnimationTransitionLibrary`, available back to Windows 7) does not — it only
//! ships fixed curves (linear, accelerate-decelerate, cubic, …). To keep Fluent's
//! exact easing while running on v1, we implement `IUIAnimationInterpolator` ourselves
//! and build transitions from it via `IUIAnimationTransitionFactory::CreateTransition`.
//!
//! This is "linear interpolation in value, warped by a cubic-bezier timing curve" —
//! the identical math v2's `CubicBezierLinear` performs, so the animation is
//! pixel-for-pixel the same on Windows 7 through 11, from a single code path.

use std::cell::Cell;

use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::UI::Animation::{
    IUIAnimationInterpolator, IUIAnimationInterpolator_Impl, IUIAnimationManager,
    IUIAnimationStoryboard, IUIAnimationTransition, IUIAnimationTransitionFactory,
    IUIAnimationVariable, UI_ANIMATION_DEPENDENCIES,
    UI_ANIMATION_DEPENDENCY_INTERMEDIATE_VALUES, UI_ANIMATION_DEPENDENCY_NONE,
};
use windows::core::{Result, implement};

/// Evaluate the cubic-bezier component (P0 = 0, P3 = 1) at parameter `t`.
fn bezier(t: f64, p1: f64, p2: f64) -> f64 {
    let mt = 1.0 - t;
    3.0 * mt * mt * t * p1 + 3.0 * mt * t * t * p2 + t * t * t
}

/// Derivative of [`bezier`] w.r.t. `t`.
fn bezier_deriv(t: f64, p1: f64, p2: f64) -> f64 {
    let mt = 1.0 - t;
    3.0 * mt * mt * p1 + 6.0 * mt * t * (p2 - p1) + 3.0 * t * t * (1.0 - p2)
}

/// Given a normalized time `x` in [0,1], find the bezier parameter `t` with
/// `bezierX(t) == x`. Newton-Raphson with a bisection fallback.
fn solve_t(x: f64, x1: f64, x2: f64) -> f64 {
    let mut t = x;
    for _ in 0..8 {
        let err = bezier(t, x1, x2) - x;
        if err.abs() < 1e-7 {
            return t;
        }
        let d = bezier_deriv(t, x1, x2);
        if d.abs() < 1e-9 {
            break;
        }
        t = (t - err / d).clamp(0.0, 1.0);
    }
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    let mut t = x;
    for _ in 0..24 {
        let xt = bezier(t, x1, x2);
        if (xt - x).abs() < 1e-7 {
            break;
        }
        if xt < x {
            lo = t;
        } else {
            hi = t;
        }
        t = 0.5 * (lo + hi);
    }
    t
}

/// The eased progress (0..1) at normalized time `u`, for curve control points
/// `[x1, y1, x2, y2]`.
fn eased_progress(u: f64, curve: [f64; 4]) -> f64 {
    let t = solve_t(u.clamp(0.0, 1.0), curve[0], curve[2]);
    bezier(t, curve[1], curve[3])
}

/// A cubic-bezier eased interpolator over a single scalar. Value goes linearly from
/// the initial value to `final_value`, with time warped by the bezier curve. Initial
/// velocity is ignored (matching v2's `CubicBezierLinear`), so retargeting starts from
/// the current value.
#[implement(IUIAnimationInterpolator)]
struct CubicBezierInterpolator {
    curve: [f64; 4],
    final_value: f64,
    initial_value: Cell<f64>,
    duration: Cell<f64>,
}

impl CubicBezierInterpolator {
    fn value_at(&self, offset: f64) -> f64 {
        let duration = self.duration.get();
        let initial = self.initial_value.get();
        if duration <= 0.0 {
            return self.final_value;
        }
        let u = (offset / duration).clamp(0.0, 1.0);
        let progress = eased_progress(u, self.curve);
        initial + progress * (self.final_value - initial)
    }
}

impl IUIAnimationInterpolator_Impl for CubicBezierInterpolator_Impl {
    fn SetInitialValueAndVelocity(&self, initial_value: f64, _initial_velocity: f64) -> Result<()> {
        self.initial_value.set(initial_value);
        Ok(())
    }

    fn SetDuration(&self, duration: f64) -> Result<()> {
        self.duration.set(duration);
        Ok(())
    }

    fn GetDuration(&self) -> Result<f64> {
        Ok(self.duration.get())
    }

    fn GetFinalValue(&self) -> Result<f64> {
        Ok(self.final_value)
    }

    fn InterpolateValue(&self, offset: f64) -> Result<f64> {
        Ok(self.value_at(offset))
    }

    fn InterpolateVelocity(&self, offset: f64) -> Result<f64> {
        // Numeric derivative of value w.r.t. time. Velocity is a hint the manager may
        // report; our transition ignores incoming velocity, so an approximation is fine.
        let duration = self.duration.get();
        if duration <= 0.0 {
            return Ok(0.0);
        }
        let eps = (duration * 1e-4).max(1e-6);
        let a = self.value_at((offset - eps).max(0.0));
        let b = self.value_at((offset + eps).min(duration));
        Ok((b - a) / (2.0 * eps))
    }

    fn GetDependencies(
        &self,
        initial_value_dependencies: *mut UI_ANIMATION_DEPENDENCIES,
        initial_velocity_dependencies: *mut UI_ANIMATION_DEPENDENCIES,
        duration_dependencies: *mut UI_ANIMATION_DEPENDENCIES,
    ) -> Result<()> {
        unsafe {
            // Final value is fixed → not dependency-affected. Intermediate values shift
            // with both the initial value and the duration. Initial velocity is ignored.
            if !initial_value_dependencies.is_null() {
                *initial_value_dependencies = UI_ANIMATION_DEPENDENCY_INTERMEDIATE_VALUES;
            }
            if !initial_velocity_dependencies.is_null() {
                *initial_velocity_dependencies = UI_ANIMATION_DEPENDENCY_NONE;
            }
            if !duration_dependencies.is_null() {
                *duration_dependencies = UI_ANIMATION_DEPENDENCY_INTERMEDIATE_VALUES;
            }
        }
        Ok(())
    }
}

/// Build a Fluent cubic-bezier eased transition to `final_value` over `duration`
/// seconds, warped by `curve` (`[x1, y1, x2, y2]`). Drop-in replacement for v2's
/// `IUIAnimationTransitionLibrary2::CreateCubicBezierLinearTransition`, backed by our
/// custom v1 interpolator so it runs on Windows 7+.
pub(crate) fn cubic_bezier_linear_transition(
    factory: &IUIAnimationTransitionFactory,
    duration: f64,
    final_value: f64,
    curve: [f64; 4],
) -> Result<IUIAnimationTransition> {
    let interpolator: IUIAnimationInterpolator = CubicBezierInterpolator {
        curve,
        final_value,
        initial_value: Cell::new(0.0),
        duration: Cell::new(duration),
    }
    .into();
    unsafe { factory.CreateTransition(&interpolator) }
}

/// An animated RGB color, as three scalar v1 animation variables (one per channel).
///
/// v2 had `CreateAnimationVectorVariable` / `GetVectorValue` to animate a color as one
/// object; v1 has no vector variables, so we drive R, G, B as three scalars under the
/// same storyboard with the same transition — the animation is identical. Alpha is
/// fixed at 1.0 (the buttons never animate alpha).
pub(crate) struct ColorVariable {
    channels: [IUIAnimationVariable; 3],
}

impl ColorVariable {
    /// Create the three channel variables initialized to `color`'s R/G/B.
    pub(crate) fn new(manager: &IUIAnimationManager, color: &D2D1_COLOR_F) -> Result<Self> {
        unsafe {
            Ok(ColorVariable {
                channels: [
                    manager.CreateAnimationVariable(color.r as f64)?,
                    manager.CreateAnimationVariable(color.g as f64)?,
                    manager.CreateAnimationVariable(color.b as f64)?,
                ],
            })
        }
    }

    /// The current interpolated color (alpha = 1.0).
    pub(crate) fn get(&self) -> Result<D2D1_COLOR_F> {
        unsafe {
            Ok(D2D1_COLOR_F {
                r: self.channels[0].GetValue()? as f32,
                g: self.channels[1].GetValue()? as f32,
                b: self.channels[2].GetValue()? as f32,
                a: 1.0,
            })
        }
    }

    /// Add an eased transition of all three channels toward `target` onto `storyboard`.
    pub(crate) fn add_color_transition(
        &self,
        storyboard: &IUIAnimationStoryboard,
        factory: &IUIAnimationTransitionFactory,
        duration: f64,
        target: &D2D1_COLOR_F,
        curve: [f64; 4],
    ) -> Result<()> {
        let finals = [target.r as f64, target.g as f64, target.b as f64];
        unsafe {
            for (channel, &final_value) in self.channels.iter().zip(finals.iter()) {
                let transition =
                    cubic_bezier_linear_transition(factory, duration, final_value, curve)?;
                storyboard.AddTransition(channel, &transition)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EASY_EASE: [f64; 4] = [0.33, 0.0, 0.67, 1.0];

    #[test]
    fn endpoints_exact() {
        assert!((eased_progress(0.0, EASY_EASE) - 0.0).abs() < 1e-6);
        assert!((eased_progress(1.0, EASY_EASE) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn symmetric_midpoint_is_half() {
        // [0.33,0,0.67,1] is symmetric, so u=0.5 eases to ~0.5.
        assert!((eased_progress(0.5, EASY_EASE) - 0.5).abs() < 1e-3);
    }

    #[test]
    fn monotonic_increasing() {
        let mut prev = -1.0;
        for i in 0..=100 {
            let p = eased_progress(i as f64 / 100.0, EASY_EASE);
            assert!(p >= prev - 1e-9, "not monotonic at {i}: {p} < {prev}");
            prev = p;
        }
    }

    #[test]
    fn ease_in_out_shape() {
        // Ease-in-out: slower than linear early (below the y=x line before the midpoint).
        assert!(eased_progress(0.25, EASY_EASE) < 0.25);
        assert!(eased_progress(0.75, EASY_EASE) > 0.75);
    }
}
