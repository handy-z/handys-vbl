use super::ball::BallInfo;
use super::shiftlock::CameraSample;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Footing {
    #[default]
    Unknown,
    Ground,
    Air,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GameState {
    pub footing: Footing,
    /// None = calibration pending
    pub cooldown: Option<bool>,
    pub ult_name: String,
    pub ult_percent: Option<f64>,
    /// the PlayerGui action bar (the footing buttons' parent) was found
    pub bar_found: bool,
    pub typing: bool,
    pub cam: Option<CameraSample>,
    /// None = no ball, net or character
    pub ball: Option<BallInfo>,
}

pub fn classify_footing(ground_visible: &[bool; 3], air_visible: &[bool; 3]) -> Footing {
    if ground_visible.iter().any(|b| *b) {
        return Footing::Ground;
    }
    if air_visible.iter().any(|b| *b) {
        return Footing::Air;
    }
    Footing::Unknown
}

/// digits[.digits] then '%', spaces trimmed.
pub fn parse_percent(s: &str) -> Option<f64> {
    // C isspace: space, \t, \n, \v, \f, \r
    let sp = |c: char| matches!(c, ' ' | '\t' | '\n' | '\x0B' | '\x0C' | '\r');
    let s = s.trim_matches(sp).strip_suffix('%')?.trim_end_matches(sp);
    let (mut dots, mut digits) = (0, 0);
    for c in s.chars() {
        if c == '.' {
            dots += 1;
        } else if c.is_ascii_digit() {
            digits += 1;
        } else {
            return None;
        }
    }
    if dots > 1 || digits == 0 || s.starts_with('.') || s.ends_with('.') {
        return None;
    }
    s.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_classify_footing() {
        assert_eq!(
            classify_footing(&[true, true, true], &[false, false, false]),
            Footing::Ground
        );
        assert_eq!(
            classify_footing(&[false, false, false], &[true, true, true]),
            Footing::Air
        );
        assert_eq!(
            classify_footing(&[false, false, false], &[false, false, false]),
            Footing::Unknown
        );
        assert_eq!(
            classify_footing(&[false, true, false], &[true, true, true]),
            Footing::Ground
        );
    }

    #[test]
    fn percent_parse_edge_cases() {
        assert_eq!(parse_percent("100%").unwrap_or(-1.0), 100.0);
        assert_eq!(parse_percent(" 42 % ").unwrap_or(-1.0), 42.0);
        assert_eq!(parse_percent("99.5%").unwrap_or(-1.0), 99.5);
        assert!(parse_percent("").is_none());
        assert!(parse_percent("MAX").is_none());
        assert!(parse_percent("100").is_none());
        assert!(parse_percent("%").is_none());
        assert!(parse_percent("1.2.3%").is_none());
    }
}
