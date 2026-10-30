use crate::model::Status;

/// Render the tray icon: a broken gauge ring ("aperture") whose colour reflects
/// the worst status across all monitored providers. The ring opens at the bottom
/// and carries a single dot at 12 o'clock — a dial you read, not a halo.
pub fn generate_dynamic_icon(status: Status) -> Vec<u8> {
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

            if in_ring && !in_bottom_gap {
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

    fn px(buf: &[u8], x: usize, y: usize) -> (u8, u8, u8, u8) {
        let i = (y * 64 + x) * 4;
        (buf[i], buf[i + 1], buf[i + 2], buf[i + 3])
    }

    #[test]
    fn aperture_has_ring_gap_and_hole() {
        let green = (16, 185, 129, 255);
        let dark = (17, 24, 39, 255);
        let b = generate_dynamic_icon(Status::Healthy);

        assert_eq!(px(&b, 12, 32), green, "left side of the ring is the status colour");
        assert_eq!(px(&b, 32, 10), green, "top of the ring is the status colour");
        assert_eq!(px(&b, 32, 52), dark, "bottom of the ring is cut away — dark fill, not green");
        assert_eq!(px(&b, 32, 32), dark, "centre hole shows the dark protective fill");
        assert_eq!(px(&b, 2, 2).3, 0, "corners stay transparent");
    }

    #[test]
    fn status_drives_colour() {
        assert_eq!(px(&generate_dynamic_icon(Status::Critical), 12, 32), (239, 68, 68, 255));
    }
}
