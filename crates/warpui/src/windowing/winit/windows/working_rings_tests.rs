use std::f32::consts::PI;

use super::*;

const SIZE: usize = 32;

fn alpha_at(pixels: &[u32], x: usize, y: usize) -> u32 {
    pixels[y * SIZE + x] >> 24
}

fn drawn(turned: f32, visible: PixelRect) -> Vec<u32> {
    let mut pixels = vec![0; SIZE * SIZE];
    rasterize(&mut pixels, SIZE, turned, visible, INK_LIGHT);
    pixels
}

#[test]
fn arc_starts_at_twelve_and_leaves_the_last_quarter_open() {
    let pixels = drawn(0., PixelRect::full(SIZE));
    // Twelve o'clock and three o'clock are on the arc; the gap lies between nine and twelve.
    assert_eq!(alpha_at(&pixels, 16, 3), 255);
    assert_eq!(alpha_at(&pixels, 28, 16), 255);
    assert_eq!(alpha_at(&pixels, 7, 7), 0);
}

#[test]
fn arc_turns_clockwise() {
    // A quarter turn later the gap sits between twelve and three.
    let pixels = drawn(PI / 2., PixelRect::full(SIZE));
    assert_eq!(alpha_at(&pixels, 24, 7), 0);
    assert_eq!(alpha_at(&pixels, 7, 7), 255);
}

#[test]
fn only_the_visible_part_is_drawn() {
    let visible = PixelRect {
        left: 0,
        top: 0,
        right: SIZE,
        bottom: SIZE / 2,
    };
    let pixels = drawn(0., visible);
    assert_eq!(alpha_at(&pixels, 16, 3), 255);
    assert_eq!(alpha_at(&pixels, 16, 28), 0);
}

#[test]
fn visible_part_is_measured_from_the_icon() {
    let icon = RectF::new(
        pathfinder_geometry::vector::vec2f(10., 20.),
        pathfinder_geometry::vector::vec2f(16., 16.),
    );
    let top_half = RectF::new(icon.origin(), pathfinder_geometry::vector::vec2f(16., 8.));
    assert_eq!(
        PixelRect::within(icon, top_half, 2., 32),
        PixelRect {
            left: 0,
            top: 0,
            right: 32,
            bottom: 16,
        }
    );
}
