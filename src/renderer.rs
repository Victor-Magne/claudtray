use crate::model::{Snapshot, Status};

/// What the colour alone cannot say, drawn in the ring's bottom gap: a solid
/// disc for "a provider is signed out", a bar for "showing stale data".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mark {
    #[default]
    None,
    Stale,
    SignedOut,
}

/// Everything the tray icon encodes: the worst-status colour plus a [`Mark`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IconState {
    pub status: Status,
    pub mark: Mark,
}

impl Default for IconState {
    fn default() -> Self {
        Self { status: Status::Healthy, mark: Mark::None }
    }
}

impl IconState {
    /// Signed out beats stale: it needs the user to act, stale heals itself.
    pub fn from_snapshot(snap: &Snapshot) -> Self {
        let mark = if snap.providers.iter().any(|p| p.auth_failed) {
            Mark::SignedOut
        } else if snap.providers.iter().any(|p| p.stale_secs.is_some()) {
            Mark::Stale
        } else {
            Mark::None
        };
        Self { status: snap.worst_status(), mark }
    }
}

const MARK_COLOR: [u8; 4] = [243, 244, 246, 255]; // #F3F4F6, reads on the dark fill

/// Render the tray icon: a broken gauge ring ("aperture") whose colour reflects
/// the worst status across all monitored providers. The ring opens at the bottom
/// and carries a single dot at 12 o'clock — a dial you read, not a halo. A
/// [`Mark`], if any, sits in the gap.
pub fn generate_dynamic_icon(state: IconState) -> Vec<u8> {
    let IconState { status, mark } = state;
    let width = 64;
    let height = 64;
    let mut pixels = vec![0u8; width * height * 4];

    // Same colour rules as the macOS ClaudeBar.
    let color = match status {
        Status::Healthy => (16, 185, 129),   // Verde  #10B981
        Status::Warning => (245, 158, 11),   // Amarelo #F59E0B
        Status::Critical => (239, 68, 68),   // Vermelho #EF4444
        Status::Depleted => (107, 114, 128), // Cinzento #6B7280
    };

    for y in 0..height {
        for x in 0..width {
            let idx = (y * width + x) * 4;
            let dx = x as i32 - 32;
            let dy = y as i32 - 32;
            let distance_sq = dx * dx + dy * dy;

            // Ring annulus, minus a ~60° wedge cut out of the bottom (the gauge
            // gap): dy>0 and |dx| < dy/sqrt(3). The bite is what turns the old
            // closed halo into a dial you read.
            let in_ring = distance_sq < 24 * 24 && distance_sq > 16 * 16;
            let in_bottom_gap = dy > 0 && dx * dx * 3 < dy * dy;

            // Marks are sized for 16-24 px: a 7 px radius disc / 17x7 px bar in
            // the 64 px grid stay >= 1.5 px after downscaling.
            let in_mark = match mark {
                Mark::None => false,
                Mark::SignedOut => dx * dx + (dy - 12) * (dy - 12) <= 7 * 7,
                Mark::Stale => dx.abs() <= 8 && (9..=15).contains(&dy),
            };

            if in_mark {
                pixels[idx..idx + 4].copy_from_slice(&MARK_COLOR);
            } else if in_ring && !in_bottom_gap {
                // Anel colorido
                pixels[idx] = color.0;
                pixels[idx + 1] = color.1;
                pixels[idx + 2] = color.2;
                pixels[idx + 3] = 255;
            } else if distance_sq <= 27 * 27 {
                // Fundo escuro protetor
                pixels[idx] = 17;
                pixels[idx + 1] = 24;
                pixels[idx + 2] = 39;
                pixels[idx + 3] = 255;
            }
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProviderSnapshot;

    const DARK: (u8, u8, u8, u8) = (17, 24, 39, 255);
    const GREEN: (u8, u8, u8, u8) = (16, 185, 129, 255);
    const LIGHT: (u8, u8, u8, u8) = (243, 244, 246, 255);

    fn px(buf: &[u8], x: usize, y: usize) -> (u8, u8, u8, u8) {
        let i = (y * 64 + x) * 4;
        (buf[i], buf[i + 1], buf[i + 2], buf[i + 3])
    }

    fn with_mark(mark: Mark) -> Vec<u8> {
        generate_dynamic_icon(IconState { status: Status::Healthy, mark })
    }

    #[test]
    fn aperture_has_ring_gap_and_hole() {
        let b = with_mark(Mark::None);

        assert_eq!(px(&b, 12, 32), GREEN, "left side of the ring is the status colour");
        assert_eq!(px(&b, 32, 10), GREEN, "top of the ring is the status colour");
        assert_eq!(px(&b, 32, 52), DARK, "bottom of the ring is cut away — dark fill, not green");
        assert_eq!(px(&b, 32, 32), DARK, "centre hole shows the dark protective fill");
        assert_eq!(px(&b, 2, 2).3, 0, "corners stay transparent");
    }

    #[test]
    fn status_drives_colour() {
        let b = generate_dynamic_icon(IconState { status: Status::Critical, mark: Mark::None });
        assert_eq!(px(&b, 12, 32), (239, 68, 68, 255));
    }

    #[test]
    fn no_mark_leaves_gap_dark() {
        let b = with_mark(Mark::None);
        assert_eq!(px(&b, 32, 44), DARK);
        assert_eq!(px(&b, 32, 38), DARK);
    }

    #[test]
    fn signed_out_draws_disc_not_bar() {
        let b = with_mark(Mark::SignedOut);
        assert_eq!(px(&b, 32, 44), LIGHT, "disc centre");
        assert_eq!(px(&b, 32, 38), LIGHT, "disc top");
        assert_eq!(px(&b, 40, 44), DARK, "disc is round: where the bar reaches stays dark");
        assert_eq!(px(&b, 12, 32), GREEN, "ring colour is untouched");
    }

    #[test]
    fn stale_draws_bar_not_disc() {
        let b = with_mark(Mark::Stale);
        assert_eq!(px(&b, 32, 44), LIGHT, "bar centre");
        assert_eq!(px(&b, 40, 44), LIGHT, "bar reaches past the disc radius");
        assert_eq!(px(&b, 32, 38), DARK, "bar is flat: disc top stays dark");
        assert_ne!(with_mark(Mark::Stale), with_mark(Mark::SignedOut));
        assert_eq!(px(&b, 12, 32), GREEN, "ring colour is untouched");
    }

    #[test]
    fn marks_stay_inside_the_backdrop() {
        for mark in [Mark::Stale, Mark::SignedOut] {
            let b = with_mark(mark);
            assert_eq!(px(&b, 2, 2).3, 0, "corners stay transparent");
            assert_eq!(px(&b, 32, 60).3, 0, "nothing drawn below the backdrop");
        }
    }

    #[test]
    fn from_snapshot_prefers_signed_out_over_stale() {
        let mut snap = Snapshot {
            updated_at: String::new(),
            theme: "dark".into(),
            language: "en".into(),
            resolved_language: "en".into(),
            providers: vec![ProviderSnapshot::unavailable("a", "A", "x")],
            catalog: Vec::new(),
            history: Default::default(),
        };
        assert_eq!(IconState::from_snapshot(&snap).mark, Mark::None);
        snap.providers[0].stale_secs = Some(5);
        assert_eq!(IconState::from_snapshot(&snap).mark, Mark::Stale);
        snap.providers.push(ProviderSnapshot::signed_out("b", "B", "x"));
        assert_eq!(IconState::from_snapshot(&snap).mark, Mark::SignedOut);
    }
}
