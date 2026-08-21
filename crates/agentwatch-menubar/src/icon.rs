//! The menu bar icon, drawn rather than shipped.
//!
//! Rasterized at runtime instead of embedded as a PNG for two reasons: there is
//! no binary asset to keep in sync with the source, and the glyph can carry
//! state — a paused collector should not look identical to a running one.
//!
//! Drawn as a macOS *template* image: every pixel is black with a coverage
//! alpha, and the system recolours it for the current menu bar. That is why
//! nothing here picks a colour — choosing one would break in dark mode, or in
//! whatever appearance the next macOS introduces.

use tray_icon::Icon;

/// Icon edge length in pixels.
///
/// The menu bar is 22 points tall; at 2x that is 44 pixels, and drawing at the
/// device resolution avoids the softness of upscaling a smaller bitmap.
const SIZE: u32 = 44;

/// What the icon should convey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Glyph {
    /// Collecting normally: a filled aperture.
    Watching,
    /// Deliberately paused: two bars.
    Paused,
    /// Daemon not running, or no readable data: a hollow ring.
    Idle,
}

/// Builds the icon for a state.
pub(crate) fn build(glyph: Glyph) -> Option<Icon> {
    let pixels = rasterize(glyph);
    Icon::from_rgba(pixels, SIZE, SIZE).ok()
}

/// Draws the glyph into RGBA bytes.
///
/// The shape is a ring with a centre mark — a lens, or an aperture. It reads at
/// 22 points, which a more literal drawing of "an agent" would not.
fn rasterize(glyph: Glyph) -> Vec<u8> {
    #[expect(clippy::cast_precision_loss, reason = "SIZE is 44")]
    let extent = SIZE as f32;
    let centre = extent / 2.0;

    // Proportions chosen so the ring stays distinct from the centre mark at
    // menu bar size; closer together and the whole thing reads as a blob.
    let outer = extent * 0.40;
    let ring_thickness = extent * 0.10;
    let pupil = extent * 0.15;
    let bar_half_width = extent * 0.055;
    let bar_half_height = extent * 0.16;
    let bar_offset = extent * 0.11;

    let mut pixels = Vec::with_capacity((SIZE * SIZE * 4) as usize);

    for y in 0..SIZE {
        for x in 0..SIZE {
            #[expect(clippy::cast_precision_loss, reason = "coordinates are below 44")]
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let (dx, dy) = (px - centre, py - centre);
            let distance = dy.mul_add(dy, dx * dx).sqrt();

            // Coverage of the ring: distance from the ring's centre line.
            let ring = coverage(ring_thickness / 2.0 - (distance - outer).abs());

            let inner = match glyph {
                Glyph::Watching => coverage(pupil - distance),
                Glyph::Idle => 0.0,
                Glyph::Paused => {
                    let left = rectangle(dx + bar_offset, dy, bar_half_width, bar_half_height);
                    let right = rectangle(dx - bar_offset, dy, bar_half_width, bar_half_height);
                    left.max(right)
                }
            };

            let alpha = ring.max(inner).clamp(0.0, 1.0);

            // An idle collector is drawn faintly. The shape still says which
            // app this is; the weight says it is not currently recording.
            let weight = if matches!(glyph, Glyph::Idle) {
                0.55
            } else {
                1.0
            };

            #[expect(clippy::cast_possible_truncation, reason = "clamped to 0..=1 above")]
            #[expect(clippy::cast_sign_loss, reason = "clamped to 0..=1 above")]
            let byte = (alpha * weight * 255.0).round() as u8;

            // Black with a coverage alpha: the contract for a template image.
            pixels.extend_from_slice(&[0, 0, 0, byte]);
        }
    }

    pixels
}

/// Antialiased coverage from a signed distance, in pixels.
///
/// Positive is inside. The one-pixel ramp is what keeps curves from looking
/// jagged at menu bar size.
fn coverage(signed_distance: f32) -> f32 {
    (signed_distance + 0.5).clamp(0.0, 1.0)
}

/// Antialiased coverage of an axis-aligned rectangle centred on the origin.
fn rectangle(dx: f32, dy: f32, half_width: f32, half_height: f32) -> f32 {
    coverage(half_width - dx.abs()).min(coverage(half_height - dy.abs()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Alpha channel of the rasterized glyph, row-major.
    fn alpha(glyph: Glyph) -> Vec<u8> {
        rasterize(glyph)
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| pixel[3])
            .collect()
    }

    /// Alpha at a pixel.
    fn at(glyph: Glyph, x: u32, y: u32) -> u8 {
        alpha(glyph)[(y * SIZE + x) as usize]
    }

    /// Prints the glyphs as text, for iterating on the shape without a build
    /// and a login. Not an assertion — run it deliberately:
    ///
    /// ```text
    /// cargo test -p agentwatch-menubar preview_the_glyphs -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "visual aid, prints rather than asserts"]
    fn preview_the_glyphs() {
        for glyph in [Glyph::Watching, Glyph::Paused, Glyph::Idle] {
            println!("\n{glyph:?}");
            let pixels = rasterize(glyph);
            // Every other row, so the aspect ratio looks right in a terminal.
            for y in (0..SIZE).step_by(2) {
                let row: String = (0..SIZE)
                    .map(|x| {
                        let alpha = pixels[((y * SIZE + x) * 4 + 3) as usize];
                        match alpha {
                            0..=25 => ' ',
                            26..=100 => '·',
                            101..=180 => '▒',
                            _ => '█',
                        }
                    })
                    .collect();
                println!("{row}");
            }
        }
    }

    #[test]
    fn the_bitmap_is_the_size_it_claims() {
        assert_eq!(rasterize(Glyph::Watching).len(), (SIZE * SIZE * 4) as usize);
    }

    #[test]
    fn every_pixel_is_black_so_macos_can_recolour_it() {
        for pixel in rasterize(Glyph::Watching).as_chunks::<4>().0 {
            assert_eq!(
                (pixel[0], pixel[1], pixel[2]),
                (0, 0, 0),
                "a template image must carry shape in alpha only"
            );
        }
    }

    #[test]
    fn watching_fills_the_centre() {
        assert_eq!(at(Glyph::Watching, SIZE / 2, SIZE / 2), 255);
    }

    #[test]
    fn idle_leaves_the_centre_empty() {
        assert_eq!(at(Glyph::Idle, SIZE / 2, SIZE / 2), 0, "a hollow ring");
    }

    #[test]
    fn paused_marks_the_centre_with_two_bars_and_a_gap_between_them() {
        let middle = SIZE / 2;
        assert_eq!(
            at(Glyph::Paused, middle, middle),
            0,
            "the gap between the bars"
        );
        assert!(at(Glyph::Paused, middle - 5, middle) > 200, "left bar");
        assert!(at(Glyph::Paused, middle + 5, middle) > 200, "right bar");
    }

    #[test]
    fn every_glyph_draws_the_same_ring() {
        // The ring is the identity of the app; only the centre carries state.
        let edge = SIZE / 2;
        for glyph in [Glyph::Watching, Glyph::Paused] {
            assert!(
                at(glyph, edge, 4) > 200,
                "{glyph:?} should have a ring at the top"
            );
        }
    }

    #[test]
    fn an_idle_collector_is_drawn_faintly() {
        let edge = SIZE / 2;
        assert!(
            at(Glyph::Idle, edge, 4) < at(Glyph::Watching, edge, 4),
            "idle should be visibly lighter than collecting"
        );
        assert!(at(Glyph::Idle, edge, 4) > 0, "but still visible");
    }

    #[test]
    fn the_corners_are_transparent() {
        for glyph in [Glyph::Watching, Glyph::Paused, Glyph::Idle] {
            assert_eq!(at(glyph, 0, 0), 0, "{glyph:?}");
            assert_eq!(at(glyph, SIZE - 1, SIZE - 1), 0, "{glyph:?}");
        }
    }

    #[test]
    fn the_edges_are_antialiased_rather_than_hard() {
        // A ring drawn without coverage ramps has only 0 and 255; partial
        // values are the evidence that it is smooth.
        let partial = alpha(Glyph::Watching)
            .iter()
            .filter(|&&a| a > 0 && a < 255)
            .count();
        assert!(
            partial > 40,
            "expected antialiased edges, found {partial} partial pixels"
        );
    }
}
