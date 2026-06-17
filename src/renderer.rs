use crate::model::Status;

/// Render the tray icon: a minimalist ring whose colour reflects the worst
/// status across all monitored providers.
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

            if distance_sq < 24 * 24 && distance_sq > 16 * 16 {
                // Anel colorido
                pixels[idx] = color.0;
                pixels[idx + 1] = color.1;
                pixels[idx + 2] = color.2;
                pixels[idx + 3] = 255;
            } else if distance_sq <= 26 * 26 {
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
