//! Content gate: refuses never-list content on every write. A refusal names the rule and stores nothing.
//! This is a pattern check. It catches known shapes, not every secret: a leaked secret is still leaked
//! and must be rotated by its owner.

use regex::Regex;
use std::sync::LazyLock;

pub const MAX_TITLE: usize = 200;
pub const MAX_TEXT: usize = 4000;

static RULES: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    [
        ("private_key_block", r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
        ("secret_token", r"\b(?:sk-|gh[opsu]_|whsec_|xox[bp]-|AKIA)[A-Za-z0-9_\-]{12,}"),
        ("secret_token", r"\b[a-z]{2,6}_[A-Za-z0-9]{24,}\b"),
        ("secret_token", r"\beyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}\."),
        // Bounded by anything but a digit, not by a word boundary: an address glued to letters, `x203.0.113.7`,
        // is still an address. Three-part numbers such as `4.2.1` never match.
        ("ip_address", r"(?:^|\P{Nd})(?:\d{1,3}\.){3}\d{1,3}(?:$|\P{Nd})"),
        // Bounded by anything but an ASCII letter or digit. Not by "any non-hex character": a to f are letters
        // too, and `end:17:40:05` is a time of day. So this catches `é2001:db8:0:5`, not `x2001:db8:0:5`.
        ("ip_address", r"(?i)(?:^|[^0-9a-z])(?:[0-9a-f]{1,4}:){3,7}[0-9a-f]{1,4}(?:$|[^0-9a-z])"),
        ("email_address", r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}"),
        ("money_amount", r"(?i)[$€£]\s?\d|\b\d+(?:[.,]\d+)?\s?(?:usd|eur|gbp|usdt|usdc)\b"),
        // The one-time code a member enrolls with (`p2p-` and six groups of four), so it is not passed on in a card.
        ("enrollment_code", r"(?i)p2p-(?:[a-z2-7]{4}-){5}[a-z2-7]{4}"),
    ]
    .into_iter()
    .map(|(name, pattern)| (name, Regex::new(pattern).expect("gate pattern compiles")))
    .collect()
});

/// Characters that print as nothing or change how text around them reads: format characters (Cf: direction
/// controls, zero-width characters, the tag block), private-use (Co) and unassigned (Cn) code points, variation
/// selectors, the combining grapheme joiner, and the Hangul fillers. The set the client removes before it prints
/// another organisation's text, less surrogates (Cs): a Rust string cannot hold one, and a lone `\ud800` escape in
/// the body is already `400 bad_json`. Unassigned means unassigned in the Unicode version this build knows.
static INVISIBLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[\p{Cf}\p{Co}\p{Cn}\x{34F}\x{115F}\x{1160}\x{180B}-\x{180F}\x{3164}\x{FFA0}\x{FE00}-\x{FE0F}\x{E0100}-\x{E01EF}]")
        .expect("invisible pattern compiles")
});

/// A letter or mark outside ASCII: what a zero-width joiner or non-joiner may stand between.
static JOINABLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[\p{L}\p{M}]$").expect("joinable pattern compiles"));

fn joinable(c: Option<char>) -> bool {
    c.is_some_and(|c| !c.is_ascii() && JOINABLE.is_match(c.encode_utf8(&mut [0; 4])))
}

/// True when the text holds an invisible character. U+200C and U+200D (zero-width non-joiner and joiner) are
/// ordinary spelling in Persian and several Indic scripts, so they pass when both neighbours are letters or
/// marks outside ASCII; anywhere else (between digits, ASCII letters or punctuation, at an end, doubled, in an
/// emoji sequence) they could split a shape the rules look for, and are refused.
fn has_invisible(text: &str) -> bool {
    INVISIBLE.find_iter(text).any(|m| {
        let c = m.as_str().chars().next();
        if !matches!(c, Some('\u{200C}' | '\u{200D}')) {
            return true;
        }
        let before = text[..m.start()].chars().next_back();
        let after = text[m.end()..].chars().next();
        !(joinable(before) && joinable(after))
    })
}

/// Returns the name of the first rule the text breaks.
pub fn check(text: &str) -> Result<(), &'static str> {
    if text.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return Err("control_character");
    }
    if has_invisible(text) {
        return Err("invisible_character");
    }
    match RULES.iter().find(|(_, rx)| rx.is_match(text)) {
        Some((name, _)) => Err(name),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::check;

    #[test]
    fn refuses_never_list_shapes() {
        assert_eq!(check("exit was 203.0.113.7 at noon"), Err("ip_address"));
        assert_eq!(check("seen 2001:db8:0:1:0:0:0:5"), Err("ip_address"));
        assert_eq!(check("key sk-abcdefghijklmnop123"), Err("secret_token"));
        assert_eq!(check("key demo_abcdefghijklmnopqrstuvwxyz12"), Err("secret_token"));
        assert_eq!(check("-----BEGIN PRIVATE KEY-----"), Err("private_key_block"));
        assert_eq!(check("mail someone@example.org"), Err("email_address"));
        assert_eq!(check("we agreed $5 per unit"), Err("money_amount"));
        assert_eq!(check("credit of 13.30 USD"), Err("money_amount"));
        assert_eq!(check("bell\u{7}"), Err("control_character"));
    }

    #[test]
    fn allows_ordinary_findings() {
        // Counts, dotted numbers and a time of day are ordinary findings, not addresses or amounts.
        assert_eq!(check("List endpoint skips one item when paging.\nPage 2 of 3 starts at item 52, expected 51. Seen in step 4.2.1 at 17:40:05 UTC."), Ok(()));
        assert_eq!(check("Docs example uses a field name the API rejects: the sample sends item_count, the API wants itemCount."), Ok(()));
    }

    #[test]
    fn refuses_invisible_and_format_characters() {
        for text in [
            "zero\u{200B}width space",          // Cf
            "\u{FEFF}byte order mark",          // Cf, and not trimmed
            "soft\u{00AD}hyphen",               // Cf
            "right\u{202E}to left override",    // Cf, a direction control
            "tag\u{E0041}\u{E0042} characters", // Cf, the tag block that spells hidden ASCII
            "private \u{E000} use",             // Co
            "noncharacter \u{FFFE}",            // Cn, permanently unassigned
            "noncharacter \u{FDD0}",            // Cn
            "grapheme\u{034F}joiner",
            "hangul\u{3164}filler",
            "hangul\u{115F}\u{1160}fillers",
            "half\u{FFA0}width filler",
            "mongolian\u{180B}selector",
            "heart \u{2764}\u{FE0F}",           // variation selector 16
            "ideograph \u{8FBB}\u{E0100}",      // ideographic variation selector
        ] {
            assert_eq!(check(text), Err("invisible_character"), "{text:?}");
        }
    }

    #[test]
    fn joiners_pass_only_between_two_non_ascii_letters_or_marks() {
        // Persian needs U+200C inside words; Devanagari uses U+200D after a virama (a mark).
        assert_eq!(check("\u{645}\u{6CC}\u{200C}\u{62E}\u{648}\u{627}\u{647}\u{645}"), Ok(()));
        assert_eq!(check("\u{915}\u{94D}\u{200D}\u{937}"), Ok(()));
        for text in [
            "203.0\u{200D}.113.7",                    // next to digits and punctuation
            "sk-ab\u{200C}cdefghijklmnop",            // between ASCII letters
            "\u{200D}\u{645}\u{6CC}",                 // at the start
            "\u{645}\u{6CC}\u{200C}",                 // at the end
            "\u{645}\u{200C}\u{200C}\u{6CC}",         // doubled
            "\u{645}\u{200C}a",                       // one neighbour ASCII
            "\u{1F468}\u{200D}\u{1F469}",             // an emoji sequence: emoji are not letters
        ] {
            assert_eq!(check(text), Err("invisible_character"), "{text:?}");
        }
    }

    #[test]
    fn ordinary_text_in_any_script_passes() {
        for text in [
            "Пагинация пропускает один элемент на второй странице.",
            "分页时第二页跳过了一项。",
            "ページ２の最初の項目が返されません。",
            "يتخطى الترقيم عنصرًا واحدًا في الصفحة الثانية.",
            "הדף השני מדלג על פריט אחד.",
            "ข้ามหนึ่งรายการเมื่อแบ่งหน้า",
            "दूसरा पेज एक आइटम छोड़ देता है।",
            "Ünïcödé ça marche, naïve façade.",
            "Works now \u{1F44D}",
        ] {
            assert_eq!(check(text), Ok(()), "{text:?}");
        }
    }

    #[test]
    fn an_address_glued_to_letters_is_still_an_address() {
        for text in ["é203.0.113.7", "x203.0.113.7", "203.0.113.7é", "_203.0.113.7_", "at\u{663}\u{660}.\u{661}.\u{662}.\u{663}", "é2001:db8:0:5", "2001:db8:0:5é", "_2001:db8:0:5"] {
            assert_eq!(check(text), Err("ip_address"), "{text:?}");
        }
        // Still let through: three-part numbers, a time of day, a long number before three dots.
        for text in ["step 4.2.1", "version 1.2.3-beta", "at 17:40:05", "x4.2.1", "1234.1.1.1 is not an address", "end:17:40:05", "time:17:40:05",
                     "page:1:2:3", "17:40:05:error"] {
            assert_eq!(check(text), Ok(()), "{text:?}");
        }
    }

    #[test]
    fn refuses_an_enrollment_code() {
        assert_eq!(check("Use p2p-abcd-abcd-abcd-abcd-abcd-abcd to join."), Err("enrollment_code"));
        assert_eq!(check("Use P2P-ABCD-EFGH-IJKL-MNOP-QRST-UVWX to join."), Err("enrollment_code"));
        assert_eq!(check("joined with xp2p-abcd-abcd-abcd-abcd-abcd-abcd2"), Err("enrollment_code"));
        assert_eq!(check("the code starts with p2p-abcd"), Ok(()));
    }
}
