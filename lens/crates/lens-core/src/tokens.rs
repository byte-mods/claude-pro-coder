//! Code-aware token estimation.
//!
//! Every lens verb caps its output by a token budget. v1 translated budgets to
//! character counts with a flat `4 chars/token` ratio, which systematically
//! under-counts dense code (punctuation-heavy lines tokenise closer to
//! 2.5–3 chars/token under BPE vocabularies such as cl100k or Claude's) and
//! over-counts prose-like doc comments. The result was budgets that looked
//! respected on paper but blew past the real limit by 20–40% on real code.
//!
//! This module replaces that with a dependency-free estimator that mimics
//! how BPE tokenisers actually split source text:
//!
//! - identifiers are split on `snake_case` / `camelCase` / digit boundaries;
//!   each sub-word costs one token up to eight characters and one extra
//!   token per ~6 additional characters when long (`Deserialize` → 2,
//!   `ensure_fresh` → 2, `x` → 1);
//! - digit runs cost one token per three digits;
//! - punctuation costs one token per character, except a small set of
//!   common two-character operators (`::`, `->`, `=>`, `==`, `!=`, `<=`,
//!   `>=`, `&&`, `||`, `//`, `/*`, `*/`, `()`, `{}`, `[]`) which merge;
//! - leading indentation costs one token per four columns (tokenisers have
//!   dedicated multi-space tokens); interior whitespace is free (it merges
//!   with the following token); every newline costs one token;
//! - non-ASCII characters cost roughly one token per two bytes (a safe
//!   over-estimate for CJK and emoji, which BPE handles byte-wise).
//!
//! The estimator is deterministic and O(n). It does not aim to reproduce
//! any one vocabulary exactly — it aims to be within ±15% across the
//! languages lens indexes so budgets are meaningful. Calibrated against
//! the lens source tree (Rust) and Python/TypeScript fixtures at
//! ~3.3 chars/token, which matches published cl100k averages for code.

/// Estimate the number of tokens a model would spend on `text`.
///
/// Never returns 0 for non-empty input; returns 0 for the empty string.
pub fn estimate_tokens(text: &str) -> u32 {
    let mut tokens: u64 = 0;
    let bytes = text.as_bytes();
    let mut i = 0usize;
    let mut at_line_start = true;

    while i < bytes.len() {
        let b = bytes[i];

        // Newline: one token, reset line-start state.
        if b == b'\n' {
            tokens += 1;
            at_line_start = true;
            i += 1;
            continue;
        }

        // Whitespace run.
        if b == b' ' || b == b'\t' || b == b'\r' {
            let start = i;
            while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t' || bytes[i] == b'\r') {
                i += 1;
            }
            if at_line_start {
                // Indentation: tabs count as 4 columns each.
                let cols: usize = bytes[start..i]
                    .iter()
                    .map(|c| if *c == b'\t' { 4 } else { 1 })
                    .sum();
                tokens += cols.div_ceil(4) as u64;
            }
            // Interior whitespace merges with the next token — free.
            continue;
        }

        at_line_start = false;

        // Identifier / word run: ASCII letters, underscores, and digits mixed in.
        if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            tokens += word_tokens(&bytes[start..i]);
            continue;
        }

        // Digit run.
        if b.is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.' || bytes[i] == b'_') {
                i += 1;
            }
            tokens += (i - start).div_ceil(3) as u64;
            continue;
        }

        // Non-ASCII: count bytes of the UTF-8 sequence, ~2 bytes per token.
        if b >= 0x80 {
            let start = i;
            while i < bytes.len() && bytes[i] >= 0x80 {
                i += 1;
            }
            tokens += (i - start).div_ceil(2) as u64;
            continue;
        }

        // Punctuation: check for a merged two-char operator first.
        if i + 1 < bytes.len() && is_merged_pair(b, bytes[i + 1]) {
            tokens += 1;
            i += 2;
            continue;
        }
        tokens += 1;
        i += 1;
    }

    tokens.min(u32::MAX as u64) as u32
}

/// Token cost of one identifier-like run. Splits on case and underscore
/// boundaries so `parseHttpRequest` and `parse_http_request` both cost the
/// same as three short words.
fn word_tokens(word: &[u8]) -> u64 {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Kind {
        None,
        Lower,
        Upper,
        Digit,
    }
    let mut total: u64 = 0;
    let mut sub_len: usize = 0;
    let mut prev = Kind::None;
    for &c in word {
        if c == b'_' {
            // Underscore always terminates the current sub-word and is
            // itself free (it merges into the neighbouring pieces).
            if sub_len > 0 {
                total += subword_cost(sub_len);
                sub_len = 0;
            }
            prev = Kind::None;
            continue;
        }
        let kind = if c.is_ascii_digit() {
            Kind::Digit
        } else if c.is_ascii_uppercase() {
            Kind::Upper
        } else {
            Kind::Lower
        };
        // Boundaries: lower→Upper (camelCase), alpha↔digit transitions.
        let boundary = match (prev, kind) {
            (Kind::Lower, Kind::Upper) => true,
            (Kind::Digit, Kind::Lower | Kind::Upper) => true,
            (Kind::Lower | Kind::Upper, Kind::Digit) => true,
            _ => false,
        };
        if boundary && sub_len > 0 {
            total += subword_cost(sub_len);
            sub_len = 0;
        }
        sub_len += 1;
        prev = kind;
    }
    if sub_len > 0 {
        total += subword_cost(sub_len);
    }
    total.max(1)
}

/// One token for sub-words up to eight characters; one more per six
/// additional characters. Mirrors BPE behaviour where common words
/// (`request`, `response`, `callback`) are single tokens and long or rare
/// identifiers fragment.
fn subword_cost(len: usize) -> u64 {
    if len <= 8 {
        1
    } else {
        1 + ((len - 8) as u64).div_ceil(6)
    }
}

fn is_merged_pair(a: u8, b: u8) -> bool {
    matches!(
        (a, b),
        (b':', b':')
            | (b'-', b'>')
            | (b'=', b'>')
            | (b'=', b'=')
            | (b'!', b'=')
            | (b'<', b'=')
            | (b'>', b'=')
            | (b'&', b'&')
            | (b'|', b'|')
            | (b'/', b'/')
            | (b'/', b'*')
            | (b'*', b'/')
            | (b'(', b')')
            | (b'{', b'}')
            | (b'[', b']')
            | (b'+', b'=')
            | (b'-', b'=')
            | (b'+', b'+')
            | (b'-', b'-')
            | (b'<', b'<')
            | (b'>', b'>')
    )
}

/// Fit `lines` into `budget_tokens`, keeping lines head-first and stopping
/// at the first line that would exceed the budget. Returns the kept lines
/// and whether anything was dropped. `reserved` tokens are subtracted from
/// the budget first (e.g. for a signature that must always be shown).
///
/// A zero (or fully reserved) budget keeps no lines and reports truncation
/// when there was any input — callers rely on that to print an "omitted"
/// note rather than silently emitting nothing.
pub fn fit_lines(lines: &[&str], budget_tokens: u32, reserved: u32) -> (Vec<String>, bool) {
    let remaining = budget_tokens.saturating_sub(reserved);
    let mut kept: Vec<String> = Vec::with_capacity(lines.len());
    let mut used: u32 = 0;
    for line in lines {
        // +1 for the newline that rejoins the line on output.
        let cost = estimate_tokens(line).saturating_add(1);
        if used.saturating_add(cost) > remaining {
            return (kept, true);
        }
        used = used.saturating_add(cost);
        kept.push((*line).to_string());
    }
    (kept, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokens_empty_string_is_zero() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn test_tokens_single_short_identifier_is_one() {
        assert_eq!(estimate_tokens("foo"), 1);
        assert_eq!(estimate_tokens("x"), 1);
    }

    #[test]
    fn test_tokens_snake_and_camel_case_split_identically() {
        let snake = estimate_tokens("parse_http_request");
        let camel = estimate_tokens("parseHttpRequest");
        assert_eq!(snake, camel, "snake={snake} camel={camel}");
        assert_eq!(snake, 3);
    }

    #[test]
    fn test_tokens_long_identifier_costs_more_than_short() {
        assert!(estimate_tokens("internationalization") > estimate_tokens("intl"));
        assert_eq!(subword_cost(8), 1);
        assert_eq!(subword_cost(9), 2);
        assert_eq!(subword_cost(14), 2);
        assert_eq!(subword_cost(15), 3);
    }

    #[test]
    fn test_tokens_merged_operators_cost_one() {
        assert_eq!(estimate_tokens("::"), 1);
        assert_eq!(estimate_tokens("->"), 1);
        assert_eq!(estimate_tokens("()"), 1);
        // Unmerged punctuation costs per char.
        assert_eq!(estimate_tokens(";"), 1);
        assert_eq!(estimate_tokens("(;"), 2);
    }

    #[test]
    fn test_tokens_newlines_and_indentation_counted() {
        // "a\n" = word + newline = 2; indentation of 8 spaces = 2 tokens.
        assert_eq!(estimate_tokens("a\n"), 2);
        assert_eq!(estimate_tokens("        a"), 3);
        // Interior spaces are free.
        assert_eq!(estimate_tokens("a b"), 2);
    }

    #[test]
    fn test_tokens_digit_runs_cost_per_three_digits() {
        assert_eq!(estimate_tokens("1"), 1);
        assert_eq!(estimate_tokens("123456"), 2);
    }

    #[test]
    fn test_tokens_non_ascii_counts_by_bytes() {
        // "é" is 2 bytes → 1 token; "日本語" is 9 bytes → 5 tokens.
        assert_eq!(estimate_tokens("é"), 1);
        assert_eq!(estimate_tokens("日本語"), 5);
    }

    #[test]
    fn test_tokens_rust_line_lands_near_expected_ratio() {
        // A realistic dense line. cl100k gives ~17 tokens for this; we want
        // to be in the same neighbourhood, not the char/4 answer (12).
        let line = "let cfg = FreshnessConfig::from_env().unwrap_or_default();";
        let t = estimate_tokens(line);
        assert!((14..=22).contains(&t), "got {t}");
    }

    #[test]
    fn test_tokens_prose_is_cheaper_per_char_than_code() {
        let prose = "The quick brown fox jumps over the lazy dog and keeps running";
        let code = "fn f(x: &[u8]) -> Result<(), E> { Ok(()) }";
        let prose_ratio = prose.len() as f64 / estimate_tokens(prose) as f64;
        let code_ratio = code.len() as f64 / estimate_tokens(code) as f64;
        assert!(prose_ratio > code_ratio, "prose={prose_ratio} code={code_ratio}");
    }

    #[test]
    fn test_fit_lines_keeps_all_when_under_budget() {
        let lines = ["let a = 1;", "let b = 2;"];
        let (kept, truncated) = fit_lines(&lines, 1000, 0);
        assert_eq!(kept.len(), 2);
        assert!(!truncated);
    }

    #[test]
    fn test_fit_lines_truncates_tail_first_when_over_budget() {
        let lines: Vec<String> = (0..50).map(|i| format!("    let var_{i:02} = {i} + {i};")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let (kept, truncated) = fit_lines(&refs, 40, 0);
        assert!(truncated);
        assert!(!kept.is_empty(), "budget of 40 should fit at least one line");
        assert!(kept.len() < 50);
        assert_eq!(kept[0], lines[0], "head-first: first line survives");
    }

    #[test]
    fn test_fit_lines_zero_budget_keeps_nothing_and_flags_truncation() {
        let (kept, truncated) = fit_lines(&["x"], 0, 0);
        assert!(kept.is_empty());
        assert!(truncated);
    }

    #[test]
    fn test_fit_lines_reserved_reduces_available_budget() {
        let lines = ["let a = 1;", "let b = 2;", "let c = 3;"];
        let (all, _) = fit_lines(&lines, 30, 0);
        let (some, truncated) = fit_lines(&lines, 30, 20);
        assert_eq!(all.len(), 3);
        assert!(some.len() < 3);
        assert!(truncated);
    }

    #[test]
    fn test_fit_lines_empty_input_is_not_truncated() {
        let (kept, truncated) = fit_lines(&[], 0, 0);
        assert!(kept.is_empty());
        assert!(!truncated);
    }
}
