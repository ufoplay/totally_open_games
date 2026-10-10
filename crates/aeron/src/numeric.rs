//! Port of `src/numeric.c`, `include/aeron/numeric.h`.
//!
//! Locale-independent `strtod` and `%.*g` with an ASCII `.` decimal separator.

/// `isspace` in the C locale.
pub(crate) fn is_c_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// Parses a complete floating-point token, like `strtod` in the C locale: leading whitespace, a sign, decimal
/// digits with an optional fraction and exponent, `inf`, `infinity`, `nan` and `nan(chars)`. Values that overflow,
/// or that underflow to zero or a subnormal, are rejected like `strtod`'s `ERANGE`.
///
/// DEVIATION: hexadecimal floats (`0x1.8p3`) are rejected; `strtod` accepts them.
#[doc(alias = "Aeron_ParseAsciiDouble")]
pub fn parse_ascii_double(text: &str) -> Option<f64> {
    let token = text.trim_start_matches(|c: char| c.is_ascii() && is_c_space(c as u8));
    let unsigned = token.strip_prefix(['+', '-']).unwrap_or(token);
    let negative = token.starts_with('-');

    let bytes = unsigned.as_bytes();
    if bytes.len() > 4 && bytes[..4].eq_ignore_ascii_case(b"nan(") && unsigned.ends_with(')') {
        let payload = &bytes[4..bytes.len() - 1];
        if payload
            .iter()
            .all(|&byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Some(if negative { -f64::NAN } else { f64::NAN });
        }
        return None;
    }
    if bytes
        .get(1)
        .is_some_and(|&byte| byte == b'x' || byte == b'X')
    {
        return None;
    }

    let value: f64 = token.parse().ok()?;
    let is_infinity_literal =
        unsigned.eq_ignore_ascii_case("inf") || unsigned.eq_ignore_ascii_case("infinity");
    if value.is_infinite() && !is_infinity_literal {
        return None;
    }
    let mantissa = unsigned.split(['e', 'E']).next().unwrap_or_default();
    let nonzero = mantissa.bytes().any(|byte| (b'1'..=b'9').contains(&byte));
    if (value == 0.0 || value.is_subnormal()) && nonzero {
        return None;
    }
    Some(value)
}

/// Removes trailing fractional zeros and a trailing `.`, as `%g` does without the `#` flag.
fn strip_trailing_zeros(text: &str) -> &str {
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.')
    } else {
        text
    }
}

/// Formats a finite value like `printf("%.*g", significant_digits, value)` in the C locale. Returns `None` for
/// non-finite values or a digit count outside `1..=17`.
#[doc(alias = "Aeron_FormatAsciiDouble")]
pub fn format_ascii_double(value: f64, significant_digits: u32) -> Option<String> {
    if !value.is_finite() || !(1..=17).contains(&significant_digits) {
        return None;
    }
    let precision = significant_digits as i32;
    let sign = if value.is_sign_negative() { "-" } else { "" };
    let magnitude = value.abs();

    // %g picks the style from the exponent of the %e conversion, after rounding.
    let exponential = format!("{magnitude:.*e}", (precision - 1) as usize);
    let (mantissa, exponent) = exponential.split_once('e')?;
    let exponent: i32 = exponent.parse().ok()?;
    let body = if exponent < -4 || exponent >= precision {
        let exponent_sign = if exponent < 0 { '-' } else { '+' };
        format!(
            "{}e{exponent_sign}{:02}",
            strip_trailing_zeros(mantissa),
            exponent.unsigned_abs()
        )
    } else {
        let fixed = format!("{magnitude:.*}", (precision - 1 - exponent) as usize);
        strip_trailing_zeros(&fixed).to_owned()
    };
    Some(format!("{sign}{body}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_like_strtod() {
        assert_eq!(parse_ascii_double("1.5"), Some(1.5));
        assert_eq!(parse_ascii_double("  -2.5e3"), Some(-2500.0));
        assert_eq!(parse_ascii_double("5."), Some(5.0));
        assert_eq!(parse_ascii_double(".25"), Some(0.25));
        assert_eq!(parse_ascii_double("+1E-2"), Some(0.01));
        assert_eq!(parse_ascii_double("0.0"), Some(0.0));
        assert_eq!(parse_ascii_double("-INF"), Some(f64::NEG_INFINITY));
        assert_eq!(parse_ascii_double("infinity"), Some(f64::INFINITY));
        assert!(parse_ascii_double("nan").unwrap().is_nan());
        assert!(parse_ascii_double("NaN(0x1)").unwrap().is_nan());
        for invalid in [
            "", ".", "1e", "1.5x", "1e400", "1e-400", "1e-310", "0x1p3", "1,5", "1.5 ",
        ] {
            assert_eq!(parse_ascii_double(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn formats_like_printf_g() {
        let cases: [(f64, u32, &str); 14] = [
            (0.0, 6, "0"),
            (-0.0, 3, "-0"),
            (1.0, 1, "1"),
            (100.0, 1, "1e+02"),
            (100.0, 3, "100"),
            (0.1, 17, "0.10000000000000001"),
            (0.1, 1, "0.1"),
            (123456.0, 6, "123456"),
            (1234567.0, 6, "1.23457e+06"),
            (0.0001, 3, "0.0001"),
            (0.00001234, 3, "1.23e-05"),
            (9.995, 3, "9.99"),
            (9.996, 3, "10"),
            (1e300, 2, "1e+300"),
        ];
        for (value, digits, expected) in cases {
            assert_eq!(
                format_ascii_double(value, digits).as_deref(),
                Some(expected),
                "{value} {digits}"
            );
        }
        assert_eq!(format_ascii_double(f64::NAN, 3), None);
        assert_eq!(format_ascii_double(1.0, 0), None);
        assert_eq!(format_ascii_double(1.0, 18), None);
    }
}
