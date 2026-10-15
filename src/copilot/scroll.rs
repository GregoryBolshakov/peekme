//! How far a turn of the wheel moves GitHub Copilot CLI's chat (1.0.98,
//! `FullscreenScrollView` and its wheel hook): 3 rows, 1 with Alt held, up
//! to either end. Copilot follows new text once it is within a row of the
//! bottom, so a turn that stops a row short of the bottom moves one more,
//! to the bottom. PageUp and PageDown move as many rows as the chat shows.

/// Rows a turn moves the chat (down is plus), `way` 1 down or -1 up.
pub fn turn(way: isize, alt: bool) -> isize {
    way * if alt { 1 } else { 3 }
}

/// The rows a run of turns (`steps`, each its own rows, one way: down)
/// moves the box's chat beyond what the turns count, given that Copilot's
/// chat moved `moved` rows and reached its bottom: one where the turn that
/// reached it moved a row more than it counts (it stopped a row short and
/// went on), else none. The turns before that one moved their full count.
pub fn snap_extra(steps: &[isize], moved: isize) -> isize {
    let mut before = 0;
    for &step in steps {
        let step = step.abs();
        // The turn that reached the bottom moved at most a row past its count.
        if before + step + 1 >= moved {
            return (moved - before - step).max(0);
        }
        before += step;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turns() {
        assert_eq!(turn(1, false), 3);
        assert_eq!(turn(-1, false), -3);
        assert_eq!(turn(-1, true), -1);
    }

    /// Copilot 4 rows from the bottom: the first turn moves 4 (a row past its
    /// 3), the rest nothing. 2 rows: the turn moves 2. Not at the bottom: the
    /// turns moved their count.
    #[test]
    fn a_turn_a_row_short_of_the_bottom_goes_on() {
        assert_eq!(snap_extra(&[3, 3, 3], 4), 1);
        assert_eq!(snap_extra(&[3, 3, 3], 7), 1);
        assert_eq!(snap_extra(&[3, 3, 3], 2), 0);
        assert_eq!(snap_extra(&[3, 3, 3], 5), 0);
        assert_eq!(snap_extra(&[3, 3, 3], 6), 0);
        assert_eq!(snap_extra(&[3, 3, 3], 9), 0);
        assert_eq!(snap_extra(&[3, 3], 0), 0);
        // A page of 32 rows that found the bottom 33 rows away.
        assert_eq!(snap_extra(&[32], 33), 1);
        // Alt: a row a turn, 2 rows away.
        assert_eq!(snap_extra(&[1, 1], 2), 1);
    }
}
