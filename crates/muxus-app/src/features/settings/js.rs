//! JavaScript number formatting the settings texts depend on.

pub use muxus_core::js::to_fixed;

/// `String(number)` for the values settings show (no exponent forms).
pub fn number_to_string(value: f64) -> String {
    if value == 0. {
        return "0".into();
    }
    format!("{value}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_ties_like_javascript() {
        assert_eq!(to_fixed(0.0625, 3), "0.063");
        assert_eq!(to_fixed(0.5, 0), "1");
        assert_eq!(to_fixed(1.005, 2), "1.00"); // 1.00499999…
        assert_eq!(to_fixed(1.1, 2), "1.10");
        assert_eq!(to_fixed(9.995, 2), "9.99"); // 9.99499999…
        assert_eq!(to_fixed(99.96, 1), "100.0");
        assert_eq!(to_fixed(-0.04, 1), "-0.0");
        assert_eq!(to_fixed(-0., 1), "0.0");
        assert_eq!(to_fixed(-1.25, 1), "-1.3");
        assert_eq!(to_fixed(5., 2), "5.00");
        assert_eq!(number_to_string(5.), "5");
        assert_eq!(number_to_string(0.063), "0.063");
    }
}
