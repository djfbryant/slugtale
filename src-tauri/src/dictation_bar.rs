//! Dictation Bar geometry (slugtale-z7a). Plain Rust so all three screen
//! positions and the click-through hit test are unit-testable without a running
//! Tauri runtime; `main.rs` supplies the live monitor and window reads.
//!
//! The bar is the Morph orb: a 44pt circle at rest that grows sideways into a
//! full pill on hover and for the whole transcribing phase. A Tauri window
//! cannot grow on hover, so the window is permanently sized for the expanded
//! state and most of it is transparent while collapsed. Transparent is still
//! clickable, so the untouched part of the window is an invisible click-trap
//! parked over the user's document — hence the paint rect and hit test here.

use crate::settings::BarPosition;

/// Logical size of the Dictation Bar window, sized for the expanded pill plus
/// the gutter its shadow needs on every side.
pub const BAR_WINDOW_WIDTH_PT: f64 = 248.0;
pub const BAR_WINDOW_HEIGHT_PT: f64 = 76.0;
/// Transparent gutter inside the window so the pill's drop shadow fades out
/// instead of being sliced off at the window edge.
///
/// This has to cover the shadow's reach: a blurred shadow extends roughly its
/// blur radius past the box, plus its offset on the side it is cast toward. At
/// `0 4px 12px` that is 16pt below and 12pt elsewhere, so 16pt covers it. The
/// gutter was 8pt against a `0 6px 22px` shadow and had never been big enough —
/// the shadow ended in a hard rectangular cut on every edge (slugtale-3d4).
pub const BAR_GUTTER_PT: f64 = 16.0;
/// Diameter of the resting orb, which is also the height of the expanded pill.
pub const BAR_ORB_SIZE_PT: f64 = 44.0;
/// Breathing room between the bar window and the edges of the display.
///
/// Paired with the gutter: what the user sees is the pill, not the window, so
/// this is `104pt - BAR_GUTTER_PT` and the pill's own edge stays 104pt off the
/// screen. Growing the gutter without taking it off here would shove the bar up
/// the screen.
pub const BAR_SCREEN_MARGIN_PT: f64 = 88.0;

/// The display the bar is being placed on, in the physical pixels Tauri reports.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MonitorGeometry {
    pub origin_x: i32,
    pub origin_y: i32,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
}

/// A rectangle in logical points, relative to the top-left of the bar window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaintRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl PaintRect {
    /// Whether a point is on the painted shape — a capsule, not its bounding
    /// box. The bar is fully rounded (`border-radius: 999px`), so at rest it is
    /// a circle in a 44pt square; claiming that square would take the four
    /// corners from the app underneath even though nothing paints there.
    fn contains(&self, x: f64, y: f64) -> bool {
        let radius = self.height / 2.0;
        // The straight run between the two end caps. Zero-length while collapsed,
        // which reduces the capsule to a circle.
        let cap_left = self.x + radius;
        let cap_right = self.x + self.width - radius;

        let dx = x - x.clamp(cap_left, cap_right);
        let dy = y - (self.y + radius);

        dx * dx + dy * dy <= radius * radius
    }
}

/// Where to put the top-left corner of the bar window on the given display, in
/// physical pixels. All three positions ride the bottom edge with the same
/// margin; left and right keep that margin horizontally too, so the bar never
/// touches a screen edge.
pub fn dictation_bar_origin(
    monitor: &MonitorGeometry,
    window_width: u32,
    window_height: u32,
    position: BarPosition,
) -> (i32, i32) {
    let margin = (BAR_SCREEN_MARGIN_PT * monitor.scale_factor) as i32;
    let screen_width = monitor.width as i32;
    let screen_height = monitor.height as i32;
    let window_width = window_width as i32;
    let window_height = window_height as i32;

    let x = match position {
        BarPosition::BottomCenter => monitor.origin_x + (screen_width - window_width) / 2,
        BarPosition::BottomLeft => monitor.origin_x + margin,
        BarPosition::BottomRight => monitor.origin_x + screen_width - window_width - margin,
    };
    let y = monitor.origin_y + screen_height - window_height - margin;

    (x, y)
}

/// The part of the bar window that actually paints, in logical points relative
/// to the window's top-left. Everything outside it is transparent, and must be
/// click-through so it does not steal input from the app underneath.
///
/// The pill hugs whichever edge the chosen position sits against, so the orb
/// lands where the user pointed it and the pill grows inwards from there.
pub fn dictation_bar_paint_rect(position: BarPosition, expanded: bool) -> PaintRect {
    let width = if expanded {
        BAR_WINDOW_WIDTH_PT - BAR_GUTTER_PT * 2.0
    } else {
        BAR_ORB_SIZE_PT
    };
    let x = match position {
        BarPosition::BottomCenter => (BAR_WINDOW_WIDTH_PT - width) / 2.0,
        BarPosition::BottomLeft => BAR_GUTTER_PT,
        BarPosition::BottomRight => BAR_WINDOW_WIDTH_PT - BAR_GUTTER_PT - width,
    };

    PaintRect {
        x,
        y: (BAR_WINDOW_HEIGHT_PT - BAR_ORB_SIZE_PT) / 2.0,
        width,
        height: BAR_ORB_SIZE_PT,
    }
}

/// Whether the pointer is over the painted part of the bar. Drives
/// `set_ignore_cursor_events`: false means the pointer is in transparent dead
/// space and its clicks belong to whatever is underneath.
pub fn pointer_is_over_dictation_bar(
    pointer: (f64, f64),
    window_origin: (i32, i32),
    scale_factor: f64,
    position: BarPosition,
    expanded: bool,
) -> bool {
    if scale_factor <= 0.0 {
        return false;
    }

    let local_x = (pointer.0 - window_origin.0 as f64) / scale_factor;
    let local_y = (pointer.1 - window_origin.1 as f64) / scale_factor;

    dictation_bar_paint_rect(position, expanded).contains(local_x, local_y)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1440x900 display at the origin, 1x, matching the numbers below.
    fn monitor() -> MonitorGeometry {
        MonitorGeometry {
            origin_x: 0,
            origin_y: 0,
            width: 1440,
            height: 900,
            scale_factor: 1.0,
        }
    }

    #[test]
    fn bottom_center_centres_the_bar_above_the_bottom_edge() {
        let (x, y) = dictation_bar_origin(&monitor(), 248, 76, BarPosition::BottomCenter);

        assert_eq!(x, (1440 - 248) / 2);
        assert_eq!(y, 900 - 76 - 88);
    }

    #[test]
    fn the_pill_sits_at_the_same_distance_from_the_screen_edge_as_its_gutter_grows() {
        // The user sees the pill, not the window, so the gutter and the screen
        // margin have to move together. Growing the gutter to fit the shadow
        // without taking it off the margin would silently raise the bar
        // (slugtale-3d4).
        let (_, y) = dictation_bar_origin(&monitor(), 248, 76, BarPosition::BottomCenter);
        let pill_bottom_from_screen_edge = 900 - (y + 76) + BAR_GUTTER_PT as i32;

        assert_eq!(pill_bottom_from_screen_edge, 104);
    }

    #[test]
    fn bottom_corners_stay_inside_the_screen_with_a_non_zero_margin() {
        let monitor = monitor();

        let (left_x, left_y) = dictation_bar_origin(&monitor, 248, 76, BarPosition::BottomLeft);
        let (right_x, right_y) = dictation_bar_origin(&monitor, 248, 76, BarPosition::BottomRight);

        assert!(left_x > 0, "left edge must not touch the screen edge");
        assert_eq!(left_x, 88);
        assert_eq!(right_x + 248, 1440 - 88);
        assert!(right_x + 248 < 1440);
        assert_eq!(left_y, right_y);
        assert!(left_y + 76 < 900);
    }

    #[test]
    fn bar_is_placed_on_the_monitor_it_was_asked_for() {
        // A second display to the right of, and above, the primary one. The bar
        // belongs on that display, not at the same desktop coordinates.
        let secondary = MonitorGeometry {
            origin_x: 1440,
            origin_y: -200,
            width: 1920,
            height: 1080,
            scale_factor: 1.0,
        };

        let (x, y) = dictation_bar_origin(&secondary, 248, 76, BarPosition::BottomCenter);

        assert_eq!(x, 1440 + (1920 - 248) / 2);
        assert_eq!(y, -200 + 1080 - 76 - 88);
    }

    #[test]
    fn margins_scale_with_the_display() {
        let retina = MonitorGeometry {
            width: 2880,
            height: 1800,
            scale_factor: 2.0,
            ..monitor()
        };

        let (x, y) = dictation_bar_origin(&retina, 496, 152, BarPosition::BottomLeft);

        assert_eq!(x, 176);
        assert_eq!(y, 1800 - 152 - 176);
    }

    #[test]
    fn the_bar_window_is_configured_at_the_size_the_hit_test_assumes() {
        // The hit test describes the paint in window-local points, so a window
        // sized differently from these constants would silently hand clicks to
        // the wrong side. tauri.conf.json is the only other place it is stated.
        let config = std::fs::read_to_string("tauri.conf.json").expect("tauri.conf.json exists");
        let config: serde_json::Value = serde_json::from_str(&config).unwrap();
        let window = config["app"]["windows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|window| window["label"] == "dictation-bar")
            .expect("the dictation-bar window is configured");

        assert_eq!(window["width"], BAR_WINDOW_WIDTH_PT);
        assert_eq!(window["height"], BAR_WINDOW_HEIGHT_PT);
    }

    #[test]
    fn collapsed_bar_paints_an_orb_against_its_chosen_edge() {
        let centre = dictation_bar_paint_rect(BarPosition::BottomCenter, false);
        let left = dictation_bar_paint_rect(BarPosition::BottomLeft, false);
        let right = dictation_bar_paint_rect(BarPosition::BottomRight, false);

        assert_eq!(centre.width, 44.0);
        assert_eq!(centre.height, 44.0);
        assert_eq!(centre.x, (248.0 - 44.0) / 2.0);
        assert_eq!(left.x, 16.0);
        assert_eq!(right.x + right.width, 248.0 - 16.0);
    }

    #[test]
    fn expanded_bar_paints_the_full_pill_whatever_the_position() {
        for position in [
            BarPosition::BottomCenter,
            BarPosition::BottomLeft,
            BarPosition::BottomRight,
        ] {
            let rect = dictation_bar_paint_rect(position, true);
            assert_eq!(
                rect.width, 216.0,
                "the pill itself is unchanged; only the gutter around it grew"
            );
            assert_eq!(rect.x, 16.0, "the pill fills the window minus its gutter");
        }
    }

    #[test]
    fn clicks_in_the_transparent_margin_are_not_the_bars_to_take() {
        // The collapsed orb leaves 90% of the window transparent. A click there
        // belongs to the document underneath, not to Slugtale.
        let far_right = (216.0, 38.0);
        let above_the_orb = (124.0, 2.0);

        assert!(!pointer_is_over_dictation_bar(
            far_right,
            (0, 0),
            1.0,
            BarPosition::BottomCenter,
            false
        ));
        assert!(!pointer_is_over_dictation_bar(
            above_the_orb,
            (0, 0),
            1.0,
            BarPosition::BottomCenter,
            false
        ));
    }

    #[test]
    fn the_corners_of_a_round_orb_are_not_part_of_it() {
        // The orb is a circle inside a 44pt square. Treating the square as
        // painted would quietly steal the four corners from the app underneath.
        let top_left_corner = (104.0, 18.0);

        assert!(!pointer_is_over_dictation_bar(
            top_left_corner,
            (0, 0),
            1.0,
            BarPosition::BottomCenter,
            false
        ));
    }

    #[test]
    fn the_rounded_ends_of_the_expanded_pill_are_not_part_of_it_either() {
        let top_left_corner = (17.0, 17.0);

        assert!(!pointer_is_over_dictation_bar(
            top_left_corner,
            (0, 0),
            1.0,
            BarPosition::BottomCenter,
            true
        ));
        // ...but the straight middle of the same edge is.
        assert!(pointer_is_over_dictation_bar(
            (124.0, 17.0),
            (0, 0),
            1.0,
            BarPosition::BottomCenter,
            true
        ));
    }

    #[test]
    fn pointer_over_the_orb_belongs_to_the_bar() {
        assert!(pointer_is_over_dictation_bar(
            (124.0, 38.0),
            (0, 0),
            1.0,
            BarPosition::BottomCenter,
            false
        ));
    }

    #[test]
    fn expanding_hands_the_rest_of_the_pill_back_to_the_bar() {
        // The same point that is dead space while collapsed is a live control
        // once the bar has grown, which is what keeps Stop and Cancel clickable.
        let over_the_controls = (216.0, 38.0);

        assert!(!pointer_is_over_dictation_bar(
            over_the_controls,
            (0, 0),
            1.0,
            BarPosition::BottomCenter,
            false
        ));
        assert!(pointer_is_over_dictation_bar(
            over_the_controls,
            (0, 0),
            1.0,
            BarPosition::BottomCenter,
            true
        ));
    }

    #[test]
    fn hit_test_reads_the_pointer_in_the_windows_own_coordinates() {
        // Pointer and window origin arrive in physical pixels on a 2x display;
        // the orb is described in logical points.
        let window_origin = (1000, 500);
        let orb_centre_physical = (1000.0 + 124.0 * 2.0, 500.0 + 38.0 * 2.0);

        assert!(pointer_is_over_dictation_bar(
            orb_centre_physical,
            window_origin,
            2.0,
            BarPosition::BottomCenter,
            false
        ));
        assert!(!pointer_is_over_dictation_bar(
            (orb_centre_physical.0, orb_centre_physical.1 + 200.0),
            window_origin,
            2.0,
            BarPosition::BottomCenter,
            false
        ));
    }
}
