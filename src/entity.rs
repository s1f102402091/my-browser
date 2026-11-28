//! 文字参照 (`&lt;` など) を元の文字に戻す (練習問題 1-4)。
//!
//! HTML には 2000 種類以上の名前付き文字参照があるが、対応表だけでバイナリとメモリを
//! 使うので、よく使うものと数値による参照 (`&#169;` / `&#xA9;`) だけに対応する。

/// `;` を探す範囲 (`&` の後ろの文字数)。対応している参照はどれもこれより短い。
const MAX_REFERENCE_LEN: usize = 32;

/// `s` の先頭 (`&` から始まる) にある文字参照を解釈し、表す文字と参照の長さ (バイト数) を返す。
///
/// 対応していない参照や、`;` で終わっていないものは `None` を返す (呼び出し側はそのまま表示する)。
pub fn decode(s: &str) -> Option<(char, usize)> {
    let rest = s.strip_prefix('&')?;
    let end = rest
        .bytes()
        .take(MAX_REFERENCE_LEN)
        .position(|b| b == b';')?;
    // `;` は ASCII なので、その位置で切っても文字の途中にはならない
    let name = &rest[..end];
    let c = match name {
        "lt" | "LT" => '<',
        "gt" | "GT" => '>',
        "amp" | "AMP" => '&',
        "quot" | "QUOT" => '"',
        "apos" => '\'',
        "nbsp" => '\u{A0}',
        _ => decode_numeric(name.strip_prefix('#')?)?,
    };
    // `&` + 名前 + `;`
    Some((c, end + 2))
}

/// 数値による参照 (`#` の後ろ。10 進数の `169` か、16 進数の `xA9`) を文字にする。
fn decode_numeric(number: &str) -> Option<char> {
    let (digits, radix) = match number.strip_prefix(['x', 'X']) {
        Some(hex) => (hex, 16),
        None => (number, 10),
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    // 0、サロゲート、U+10FFFF より大きい値は文字として使えないので U+FFFD にする (HTML の仕様と同じ)
    let c = u32::from_str_radix(digits, radix)
        .ok()
        .and_then(char::from_u32)
        .filter(|&c| c != '\0')
        .map_or(char::REPLACEMENT_CHARACTER, windows_1252_fallback);
    Some(c)
}

/// 0x80〜0x9F (C1 制御文字) を、Windows-1252 で同じ番号の文字に読み替える (HTML の仕様の対応表)。
///
/// 古いページは `&#146;` (’) や `&#150;` (–) のように、Windows-1252 の番号で書いていることが多い。
fn windows_1252_fallback(c: char) -> char {
    match u32::from(c) {
        0x80 => '€',
        0x82 => '‚',
        0x83 => 'ƒ',
        0x84 => '„',
        0x85 => '…',
        0x86 => '†',
        0x87 => '‡',
        0x88 => 'ˆ',
        0x89 => '‰',
        0x8A => 'Š',
        0x8B => '‹',
        0x8C => 'Œ',
        0x8E => 'Ž',
        0x91 => '‘',
        0x92 => '’',
        0x93 => '“',
        0x94 => '”',
        0x95 => '•',
        0x96 => '–',
        0x97 => '—',
        0x98 => '˜',
        0x99 => '™',
        0x9A => 'š',
        0x9B => '›',
        0x9C => 'œ',
        0x9E => 'ž',
        0x9F => 'Ÿ',
        // 0x81・0x8D・0x8F・0x90・0x9D と、それ以外の文字はそのまま
        _ => c,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_named_references() {
        assert_eq!(decode("&lt;div"), Some(('<', 4)));
        assert_eq!(decode("&gt;"), Some(('>', 4)));
        assert_eq!(decode("&amp;lt;"), Some(('&', 5)));
        assert_eq!(decode("&quot;"), Some(('"', 6)));
        assert_eq!(decode("&apos;"), Some(('\'', 6)));
        assert_eq!(decode("&nbsp;"), Some(('\u{A0}', 6)));
        assert_eq!(decode("&LT;"), Some(('<', 4)));
    }

    #[test]
    fn decodes_numeric_references() {
        assert_eq!(decode("&#169;"), Some(('©', 6)));
        assert_eq!(decode("&#xA9;"), Some(('©', 6)));
        assert_eq!(decode("&#X1F600;"), Some(('😀', 9)));
        assert_eq!(decode("&#0065;"), Some(('A', 7)));
    }

    #[test]
    fn maps_c1_references_like_windows_1252() {
        assert_eq!(decode("&#146;"), Some(('’', 6)));
        assert_eq!(decode("&#x96;"), Some(('–', 6)));
        assert_eq!(decode("&#128;"), Some(('€', 6)));
        // 対応表にないものはそのまま (表示するときに置き換える)
        assert_eq!(decode("&#x81;"), Some(('\u{81}', 6)));
    }

    #[test]
    fn replaces_invalid_code_points() {
        for reference in ["&#0;", "&#xD800;", "&#x110000;", "&#99999999999999999999;"] {
            assert_eq!(
                decode(reference).map(|(c, _)| c),
                Some(char::REPLACEMENT_CHARACTER),
                "{reference}"
            );
        }
    }

    #[test]
    fn ignores_unknown_or_unterminated_references() {
        for text in [
            "&",
            "&;",
            "&lt",
            "&unknown;",
            "&#;",
            "&#x;",
            "&#12a;",
            "&#xZZ;",
            "&Lt;",
        ] {
            assert_eq!(decode(text), None, "{text}");
        }
        // `;` が遠すぎるものは参照とみなさない
        assert_eq!(
            decode(&format!("&{};", "a".repeat(MAX_REFERENCE_LEN))),
            None
        );
    }
}
