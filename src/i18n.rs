//! Minimal, dependency-free i18n for the tray menu, notifications and tooltip.
//!
//! The dashboard webview has its own translation table in `ui/dashboard.js`
//! (keyed by the same "pt"/"en" codes); this module only covers the Rust-side
//! surfaces that never go through the webview.

/// Supported UI languages. Add a variant + arm in every `match` below to add one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Pt,
    En,
}

impl Lang {
    /// Parse a persisted preference: "system" resolves against the OS locale;
    /// anything else must be a known language code ("pt", "en"), falling back
    /// to Portuguese (the app's original default) if unrecognised.
    pub fn from_pref(pref: &str) -> Self {
        match pref {
            "en" => Lang::En,
            "pt" => Lang::Pt,
            "system" => Self::detect_system(),
            _ => Lang::Pt,
        }
    }

    /// The BCP-47-ish short code used in state.json / Snapshot / dashboard.js.
    pub fn code(self) -> &'static str {
        match self {
            Lang::Pt => "pt",
            Lang::En => "en",
        }
    }

    #[cfg(target_os = "linux")]
    fn detect_system() -> Self {
        for var in ["LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"] {
            if let Ok(val) = std::env::var(var) {
                if let Some(lang) = Self::from_locale_code(&val) {
                    return lang;
                }
            }
        }
        Lang::Pt
    }

    #[cfg(windows)]
    fn detect_system() -> Self {
        use std::os::windows::ffi::OsStringExt;
        use winapi::um::winnls::GetUserDefaultLocaleName;

        const LOCALE_NAME_MAX_LENGTH: usize = 85;
        let mut buf = [0u16; LOCALE_NAME_MAX_LENGTH];
        let len = unsafe {
            GetUserDefaultLocaleName(buf.as_mut_ptr(), LOCALE_NAME_MAX_LENGTH as i32)
        };
        if len <= 0 {
            return Lang::Pt;
        }
        let os_string = std::ffi::OsString::from_wide(&buf[..(len as usize - 1)]);
        let locale = os_string.to_string_lossy();
        Self::from_locale_code(&locale).unwrap_or(Lang::Pt)
    }

    /// Extract the primary language subtag from a locale string like
    /// "pt_BR.UTF-8", "pt-PT", "en-US" and map it to a supported `Lang`.
    fn from_locale_code(code: &str) -> Option<Self> {
        let primary = code
            .split(['_', '-', '.'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        match primary.as_str() {
            "pt" => Some(Lang::Pt),
            "en" => Some(Lang::En),
            _ => None,
        }
    }
}

/// Rust-side strings that never pass through the webview.
pub struct Catalog {
    pub menu_show: &'static str,
    pub menu_refresh: &'static str,
    pub menu_exit: &'static str,
    pub tray_loading: &'static str,
    pub alert_critical_title: &'static str,
    pub alert_depleted_title: &'static str,
    pub alert_generic_title: &'static str,
    pub alert_body_exhausted: &'static str,
    pub alert_body_remaining: &'static str,
    pub tooltip_session: &'static str,
    pub tooltip_weekly: &'static str,
    pub tooltip_default: &'static str,
    /// Claude provider note when the OAuth usage endpoint 429s. `{mins}` is
    /// replaced with the retry-after estimate, rounded up to whole minutes.
    pub provider_rate_limited: &'static str,
}

const PT: Catalog = Catalog {
    menu_show: "Mostrar painel",
    menu_refresh: "Atualizar",
    menu_exit: "Sair",
    tray_loading: "ClaudTray — a carregar…",
    alert_critical_title: "ClaudTray — Quota Crítica",
    alert_depleted_title: "ClaudTray — Quota Esgotada",
    alert_generic_title: "ClaudTray — Alerta",
    alert_body_exhausted: "{provider} {label}: esgotado",
    alert_body_remaining: "{provider} {label}: {pct}% restante",
    tooltip_session: "SESSÃO",
    tooltip_weekly: "SEMANAL",
    tooltip_default: "ClaudTray — Monitor de Uso de IA",
    provider_rate_limited: "Limite da API atingido — tenta em ~{mins} min",
};

const EN: Catalog = Catalog {
    menu_show: "Show dashboard",
    menu_refresh: "Refresh",
    menu_exit: "Exit",
    tray_loading: "ClaudTray — loading…",
    alert_critical_title: "ClaudTray — Critical Quota",
    alert_depleted_title: "ClaudTray — Quota Exhausted",
    alert_generic_title: "ClaudTray — Alert",
    alert_body_exhausted: "{provider} {label}: exhausted",
    alert_body_remaining: "{provider} {label}: {pct}% remaining",
    tooltip_session: "SESSION",
    tooltip_weekly: "WEEKLY",
    tooltip_default: "ClaudTray — AI Usage Monitor",
    provider_rate_limited: "API limit reached — try again in ~{mins} min",
};

pub fn catalog(lang: Lang) -> &'static Catalog {
    match lang {
        Lang::Pt => &PT,
        Lang::En => &EN,
    }
}
