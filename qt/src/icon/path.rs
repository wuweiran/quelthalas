//! Turn a Fluent icon's SVG `<path d="…">` into an `ID2D1PathGeometry`.
//!
//! Our icons are each a single filled `<path>` (verified: no `<circle>`, `<g>`,
//! gradients, or strokes anywhere in the set). `ID2D1DeviceContext5::CreateSvgDocument`
//! — the API we used to render them — is Windows 10 1703+. A `PathGeometry` filled
//! with a solid brush is **Direct2D 1.0**, so it renders identically from Windows 7
//! through 11, and it lets the caller pick the tint at *draw* time (a brush) instead
//! of baking it into the document. This removes the 1703 requirement for icons.
//!
//! The grammar covers what the Fluent set actually uses — `M L H V C Z` plus relative
//! variants and elliptical arcs (`A/a`) — and, for pasting arbitrary Fluent strings,
//! smooth/quadratic curves (`S Q T`). Fill mode is winding (SVG's nonzero default), so
//! the presence glyphs' knocked-out holes render correctly.

use windows::Win32::Graphics::Direct2D::Common::{
    D2D_SIZE_F, D2D1_BEZIER_SEGMENT, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED,
    D2D1_FIGURE_END_OPEN, D2D1_FILL_MODE_WINDING,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL, D2D1_SWEEP_DIRECTION_CLOCKWISE,
    D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE, ID2D1Factory1, ID2D1PathGeometry1,
};
use windows::core::Result;
use windows_numerics::Vector2;

use crate::icon::Icon;

/// One absolute-coordinate drawing step. The tokenizer resolves all relative commands
/// and reflections into these, so the geometry builder stays trivial.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Seg {
    /// Start a new sub-path (SVG `M`).
    Begin(f32, f32),
    /// Straight line to (SVG `L/H/V`).
    Line(f32, f32),
    /// Cubic Bézier: (c1x, c1y, c2x, c2y, x, y).
    Cubic(f32, f32, f32, f32, f32, f32),
    /// Elliptical arc to (x, y).
    Arc {
        rx: f32,
        ry: f32,
        rot: f32,
        large: bool,
        sweep: bool,
        x: f32,
        y: f32,
    },
    /// Close the current sub-path (SVG `Z`).
    Close,
}

/// Pull the `d` attribute value out of a single-`<path>` SVG document. The leading
/// space in `" d=\""` avoids matching other attributes that end in `d` (e.g. `id=`).
fn extract_d(svg: &str) -> Option<&str> {
    let start = svg.find(" d=\"")? + 4;
    let rest = &svg[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// A byte cursor over the `d` string with SVG-flavored number scanning.
struct Cursor<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Cursor<'a> {
    fn new(s: &'a str) -> Self {
        Cursor { b: s.as_bytes(), i: 0 }
    }

    fn skip_sep(&mut self) {
        while let Some(&c) = self.b.get(self.i) {
            match c {
                b' ' | b',' | b'\t' | b'\n' | b'\r' => self.i += 1,
                _ => break,
            }
        }
    }

    fn at_end(&mut self) -> bool {
        self.skip_sep();
        self.i >= self.b.len()
    }

    /// A command letter if one is next (does not consume separators permanently past it).
    fn peek_cmd(&mut self) -> Option<u8> {
        self.skip_sep();
        self.b.get(self.i).copied().filter(|c| c.is_ascii_alphabetic())
    }

    /// Scan one SVG number (optional sign, decimal, exponent). `None` at a letter/end.
    fn number(&mut self) -> Option<f32> {
        self.skip_sep();
        let start = self.i;
        if let Some(&c) = self.b.get(self.i) {
            if c == b'+' || c == b'-' {
                self.i += 1;
            }
        }
        let mut seen_dot = false;
        let mut seen_digit = false;
        while let Some(&c) = self.b.get(self.i) {
            match c {
                b'0'..=b'9' => {
                    seen_digit = true;
                    self.i += 1;
                }
                b'.' if !seen_dot => {
                    seen_dot = true;
                    self.i += 1;
                }
                b'e' | b'E' => {
                    self.i += 1;
                    if let Some(&s) = self.b.get(self.i) {
                        if s == b'+' || s == b'-' {
                            self.i += 1;
                        }
                    }
                }
                _ => break,
            }
        }
        if !seen_digit {
            self.i = start;
            return None;
        }
        std::str::from_utf8(&self.b[start..self.i]).ok()?.parse().ok()
    }

    /// An arc flag: exactly one `0` or `1`, no separator needed (SVG packs them, e.g.
    /// `a.5.5 0 0 1 .7.7`).
    fn flag(&mut self) -> Option<bool> {
        self.skip_sep();
        match self.b.get(self.i).copied() {
            Some(b'0') => {
                self.i += 1;
                Some(false)
            }
            Some(b'1') => {
                self.i += 1;
                Some(true)
            }
            _ => None,
        }
    }
}

/// Tokenize a path `d` string into absolute-coordinate [`Seg`]s. Malformed input stops
/// the scan early rather than erroring — a partial glyph beats a hard failure.
fn parse(d: &str) -> Vec<Seg> {
    let mut cur = Cursor::new(d);
    let mut out = Vec::new();
    let (mut cx, mut cy) = (0f32, 0f32); // current point
    let (mut sx, mut sy) = (0f32, 0f32); // sub-path start
    // Last cubic/quad control point, for S/T reflection.
    let (mut pcx, mut pcy) = (0f32, 0f32);
    let mut prev_cubic = false;
    let mut prev_quad = false;
    let mut cmd = 0u8;

    while !cur.at_end() {
        if let Some(c) = cur.peek_cmd() {
            cmd = c;
            cur.i += 1;
        }
        let rel = cmd.is_ascii_lowercase();
        let up = cmd.to_ascii_uppercase();
        let (mut this_cubic, mut this_quad) = (false, false);

        match up {
            b'M' => {
                let (Some(mut x), Some(mut y)) = (cur.number(), cur.number()) else { break };
                if rel {
                    x += cx;
                    y += cy;
                }
                out.push(Seg::Begin(x, y));
                cx = x;
                cy = y;
                sx = x;
                sy = y;
                // Extra coordinate pairs after an M are implicit L (relative to the
                // running current point).
                while cur.peek_cmd().is_none() && !cur.at_end() {
                    let (Some(mut lx), Some(mut ly)) = (cur.number(), cur.number()) else { break };
                    if rel {
                        lx += cx;
                        ly += cy;
                    }
                    out.push(Seg::Line(lx, ly));
                    cx = lx;
                    cy = ly;
                }
            }
            b'L' => {
                while cur.peek_cmd().is_none() && !cur.at_end() {
                    let (Some(mut x), Some(mut y)) = (cur.number(), cur.number()) else { break };
                    if rel {
                        x += cx;
                        y += cy;
                    }
                    out.push(Seg::Line(x, y));
                    cx = x;
                    cy = y;
                }
            }
            b'H' => {
                while cur.peek_cmd().is_none() && !cur.at_end() {
                    let Some(mut x) = cur.number() else { break };
                    if rel {
                        x += cx;
                    }
                    out.push(Seg::Line(x, cy));
                    cx = x;
                }
            }
            b'V' => {
                while cur.peek_cmd().is_none() && !cur.at_end() {
                    let Some(mut y) = cur.number() else { break };
                    if rel {
                        y += cy;
                    }
                    out.push(Seg::Line(cx, y));
                    cy = y;
                }
            }
            b'C' => {
                while cur.peek_cmd().is_none() && !cur.at_end() {
                    let coords = [
                        cur.number(), cur.number(), cur.number(),
                        cur.number(), cur.number(), cur.number(),
                    ];
                    let [Some(mut a), Some(mut b), Some(mut c), Some(mut d), Some(mut e), Some(mut f)] =
                        coords else { break };
                    if rel {
                        a += cx; b += cy; c += cx; d += cy; e += cx; f += cy;
                    }
                    out.push(Seg::Cubic(a, b, c, d, e, f));
                    pcx = c;
                    pcy = d;
                    cx = e;
                    cy = f;
                    this_cubic = true;
                }
            }
            b'S' => {
                // Smooth cubic: first control point is the reflection of the previous
                // cubic's second control point about the current point.
                while cur.peek_cmd().is_none() && !cur.at_end() {
                    let coords = [cur.number(), cur.number(), cur.number(), cur.number()];
                    let [Some(mut c), Some(mut d), Some(mut e), Some(mut f)] = coords else { break };
                    if rel {
                        c += cx; d += cy; e += cx; f += cy;
                    }
                    let (a, b) = if prev_cubic { (2.0 * cx - pcx, 2.0 * cy - pcy) } else { (cx, cy) };
                    out.push(Seg::Cubic(a, b, c, d, e, f));
                    pcx = c;
                    pcy = d;
                    cx = e;
                    cy = f;
                    this_cubic = true;
                }
            }
            b'Q' => {
                // Quadratic → elevate to cubic for D2D.
                while cur.peek_cmd().is_none() && !cur.at_end() {
                    let coords = [cur.number(), cur.number(), cur.number(), cur.number()];
                    let [Some(mut qx), Some(mut qy), Some(mut x), Some(mut y)] = coords else { break };
                    if rel {
                        qx += cx; qy += cy; x += cx; y += cy;
                    }
                    let (c1x, c1y) = (cx + 2.0 / 3.0 * (qx - cx), cy + 2.0 / 3.0 * (qy - cy));
                    let (c2x, c2y) = (x + 2.0 / 3.0 * (qx - x), y + 2.0 / 3.0 * (qy - y));
                    out.push(Seg::Cubic(c1x, c1y, c2x, c2y, x, y));
                    pcx = qx;
                    pcy = qy;
                    cx = x;
                    cy = y;
                    this_quad = true;
                }
            }
            b'T' => {
                // Smooth quadratic: control is reflection of previous quad control.
                while cur.peek_cmd().is_none() && !cur.at_end() {
                    let (Some(mut x), Some(mut y)) = (cur.number(), cur.number()) else { break };
                    if rel {
                        x += cx;
                        y += cy;
                    }
                    let (qx, qy) = if prev_quad { (2.0 * cx - pcx, 2.0 * cy - pcy) } else { (cx, cy) };
                    let (c1x, c1y) = (cx + 2.0 / 3.0 * (qx - cx), cy + 2.0 / 3.0 * (qy - cy));
                    let (c2x, c2y) = (x + 2.0 / 3.0 * (qx - x), y + 2.0 / 3.0 * (qy - y));
                    out.push(Seg::Cubic(c1x, c1y, c2x, c2y, x, y));
                    pcx = qx;
                    pcy = qy;
                    cx = x;
                    cy = y;
                    this_quad = true;
                }
            }
            b'A' => {
                while cur.peek_cmd().is_none() && !cur.at_end() {
                    let rx = cur.number();
                    let ry = cur.number();
                    let rot = cur.number();
                    let large = cur.flag();
                    let sweep = cur.flag();
                    let x = cur.number();
                    let y = cur.number();
                    let (Some(rx), Some(ry), Some(rot), Some(large), Some(sweep), Some(mut x), Some(mut y)) =
                        (rx, ry, rot, large, sweep, x, y) else { break };
                    if rel {
                        x += cx;
                        y += cy;
                    }
                    out.push(Seg::Arc { rx, ry, rot, large, sweep, x, y });
                    cx = x;
                    cy = y;
                }
            }
            b'Z' => {
                out.push(Seg::Close);
                cx = sx;
                cy = sy;
            }
            _ => break, // unknown command — stop rather than misparse
        }

        prev_cubic = this_cubic;
        prev_quad = this_quad;
    }
    out
}

fn pt(x: f32, y: f32) -> Vector2 {
    Vector2 { X: x, Y: y }
}

/// Build a fillable geometry for `icon` in its native coordinate space (0..`icon.size`).
/// The caller scales/translates via the render target's transform and fills with any
/// brush — the tint is chosen at draw time.
pub(crate) fn build_geometry(factory: &ID2D1Factory1, icon: &Icon) -> Result<ID2D1PathGeometry1> {
    let svg = unsafe { std::str::from_utf8(icon.svg.as_bytes()).unwrap_or("") };
    let segs = extract_d(svg).map(parse).unwrap_or_default();

    unsafe {
        let geometry = factory.CreatePathGeometry()?;
        let sink = geometry.Open()?;
        sink.SetFillMode(D2D1_FILL_MODE_WINDING);

        let mut open = false;
        let mut last = (0f32, 0f32);
        // Ensure a figure is open before a draw step (SVG allows draw ops right after Z).
        macro_rules! ensure_open {
            () => {
                if !open {
                    sink.BeginFigure(pt(last.0, last.1), D2D1_FIGURE_BEGIN_FILLED);
                    open = true;
                }
            };
        }

        for seg in segs {
            match seg {
                Seg::Begin(x, y) => {
                    if open {
                        sink.EndFigure(D2D1_FIGURE_END_OPEN);
                    }
                    sink.BeginFigure(pt(x, y), D2D1_FIGURE_BEGIN_FILLED);
                    open = true;
                    last = (x, y);
                }
                Seg::Line(x, y) => {
                    ensure_open!();
                    sink.AddLine(pt(x, y));
                    last = (x, y);
                }
                Seg::Cubic(a, b, c, d, e, f) => {
                    ensure_open!();
                    sink.AddBezier(&D2D1_BEZIER_SEGMENT {
                        point1: pt(a, b),
                        point2: pt(c, d),
                        point3: pt(e, f),
                    });
                    last = (e, f);
                }
                Seg::Arc { rx, ry, rot, large, sweep, x, y } => {
                    ensure_open!();
                    sink.AddArc(&D2D1_ARC_SEGMENT {
                        point: pt(x, y),
                        size: D2D_SIZE_F { width: rx, height: ry },
                        rotationAngle: rot,
                        sweepDirection: if sweep {
                            D2D1_SWEEP_DIRECTION_CLOCKWISE
                        } else {
                            D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE
                        },
                        arcSize: if large { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
                    });
                    last = (x, y);
                }
                Seg::Close => {
                    if open {
                        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                        open = false;
                    }
                }
            }
        }
        if open {
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
        }
        sink.Close()?;
        Ok(geometry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: Seg, b: Seg) -> bool {
        fn c(x: f32, y: f32) -> bool {
            (x - y).abs() < 1e-4
        }
        match (a, b) {
            (Seg::Begin(x1, y1), Seg::Begin(x2, y2)) | (Seg::Line(x1, y1), Seg::Line(x2, y2)) => {
                c(x1, x2) && c(y1, y2)
            }
            (Seg::Cubic(a1, b1, c1, d1, e1, f1), Seg::Cubic(a2, b2, c2, d2, e2, f2)) => {
                c(a1, a2) && c(b1, b2) && c(c1, c2) && c(d1, d2) && c(e1, e2) && c(f1, f2)
            }
            _ => a == b,
        }
    }

    #[test]
    fn extracts_d_not_id() {
        let svg = r##"<svg width="12"><path d="M1 2Z" fill="#212121"/></svg>"##;
        assert_eq!(extract_d(svg), Some("M1 2Z"));
    }

    #[test]
    fn checkmark_absolute_cubics() {
        // checkmark_12_filled
        let d = "M9.76497 3.20474C10.0661 3.48915 10.0797 3.96383 9.79526 4.26497L5.54526 8.76497C5.40613 8.91228 5.21332 8.99703 5.01071 8.99993C4.8081 9.00282 4.61295 8.92361 4.46967 8.78033L2.21967 6.53033C1.92678 6.23744 1.92678 5.76257 2.21967 5.46967C2.51256 5.17678 2.98744 5.17678 3.28033 5.46967L4.98463 7.17397L8.70474 3.23503C8.98915 2.9339 9.46383 2.92033 9.76497 3.20474Z";
        let segs = parse(d);
        assert_eq!(segs[0], Seg::Begin(9.76497, 3.20474));
        assert_eq!(*segs.last().unwrap(), Seg::Close);
        // 1 Begin + 8 cubics + a couple lines + close; must have >= 1 cubic and no panic.
        assert!(segs.iter().any(|s| matches!(s, Seg::Cubic(..))));
    }

    #[test]
    fn chevron_relative_and_arcs() {
        // chevron_down_16 uses relative c/l and three relative arcs with packed flags.
        let d = "M3.15 5.65c.2-.2.5-.2.7 0L8 9.79l4.15-4.14a.5.5 0 0 1 .7.7l-4.5 4.5a.5.5 0 0 1-.7 0l-4.5-4.5a.5.5 0 0 1 0-.7Z";
        let segs = parse(d);
        assert_eq!(segs[0], Seg::Begin(3.15, 5.65));
        // relative cubic resolved to absolute: c .2 -.2 .5 -.2 .7 0 from (3.15,5.65)
        assert!(approx(segs[1], Seg::Cubic(3.35, 5.45, 3.65, 5.45, 3.85, 5.65)));
        // exactly three arcs, all resolved to absolute endpoints
        let arcs: Vec<_> = segs.iter().filter(|s| matches!(s, Seg::Arc { .. })).collect();
        assert_eq!(arcs.len(), 3);
        if let Seg::Arc { rx, ry, large, sweep, x, y, .. } = *arcs[0] {
            assert_eq!((rx, ry), (0.5, 0.5));
            assert!(!large && sweep); // flags "0 1"
            // current point before this arc is (12.15, 5.65) after `L8 9.79 l4.15-4.14`;
            // relative endpoint (.7,.7) → (12.85, 6.35).
            assert!((x - 12.85).abs() < 1e-4 && (y - 6.35).abs() < 1e-4);
        }
        assert_eq!(*segs.last().unwrap(), Seg::Close);
    }

    #[test]
    fn implicit_lineto_after_moveto() {
        let segs = parse("M1 1 2 2 3 3");
        assert_eq!(segs[0], Seg::Begin(1.0, 1.0));
        assert_eq!(segs[1], Seg::Line(2.0, 2.0));
        assert_eq!(segs[2], Seg::Line(3.0, 3.0));
    }

    #[test]
    fn packed_decimals() {
        // ".5.5" is two numbers; "-.7 0" is a signed implicit-zero pair.
        let segs = parse("M0 0l.5.5-.7 0");
        assert_eq!(segs[0], Seg::Begin(0.0, 0.0));
        assert!(approx(segs[1], Seg::Line(0.5, 0.5)));
        assert!(approx(segs[2], Seg::Line(-0.2, 0.5)));
    }
}
