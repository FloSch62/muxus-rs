//! Colors the app stores as content (never chrome): the colors offered for hosts and the
//! colors of the documentation's demo data (`hack/demo-env.mjs`).

/// `HOST_COLORS` of `host-organization.ts`: what the host organizer offers.
pub const HOST_COLORS: [(&str, &str); 10] = [
    ("Red", "#ef5350"),
    ("Orange", "#ff9800"),
    ("Amber", "#f6c344"),
    ("Green", "#4caf72"),
    ("Teal", "#26a69a"),
    ("Cyan", "#29b6c8"),
    ("Blue", "#4285f4"),
    ("Indigo", "#6e78e8"),
    ("Purple", "#ab68d4"),
    ("Pink", "#ec5b91"),
];

/// The colors of the documentation's demo data.
pub mod demo {
    pub const RED: &str = "#ef4444";
    pub const ORANGE: &str = "#f97316";
    pub const YELLOW: &str = "#eab308";
    pub const GREEN: &str = "#22c55e";
    pub const BLUE: &str = "#3b82f6";
    pub const PURPLE: &str = "#a855f7";
}
