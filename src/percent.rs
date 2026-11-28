//! URL のパーセントエンコーディング (`%XX` の形。RFC 3986 2.1 節) の変換。

const HEX_DIGITS: &[u8; 16] = b"0123456789ABCDEF";

/// `s` を `out` に書き足す。ASCII 以外のバイトは `%XX` に変換し、ASCII の文字はそのまま残す
/// (例: `日` → `%E6%97%A5`)。
pub fn encode_non_ascii(s: &str, out: &mut String) {
    for &b in s.as_bytes() {
        if b.is_ascii() {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(HEX_DIGITS[usize::from(b >> 4)]));
            out.push(char::from(HEX_DIGITS[usize::from(b & 0x0F)]));
        }
    }
}

/// `%XX` を元のバイトに戻す。
///
/// `%` の後ろに 16 進数が 2 桁続かないときは、その `%` をそのまま残す
/// (WHATWG の URL Standard と同じ。`100%` のような書き方も表示できる)。
pub fn decode(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(b) = hex_pair(&bytes[i + 1..])
        {
            decoded.push(b);
            i += 3;
            continue;
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    decoded
}

/// すべての `%` の後ろに 16 進数が 2 桁続いているか。
pub fn is_well_formed(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes
        .iter()
        .enumerate()
        .all(|(i, &b)| b != b'%' || hex_pair(&bytes[i + 1..]).is_some())
}

/// 先頭の 2 バイトを 16 進数 2 桁として読む。
fn hex_pair(bytes: &[u8]) -> Option<u8> {
    match bytes {
        [high, low, ..] => Some((hex_value(*high)? << 4) | hex_value(*low)?),
        _ => None,
    }
}

/// 16 進数 1 桁の値。
fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_only_non_ascii_bytes() {
        let mut out = String::new();
        encode_non_ascii("/wiki/日本?q=a b", &mut out);
        assert_eq!(out, "/wiki/%E6%97%A5%E6%9C%AC?q=a b");
    }

    #[test]
    fn decodes_escapes() {
        assert_eq!(decode("a%20b%2fc%E6%97%A5"), "a b/c日".as_bytes());
    }

    #[test]
    fn keeps_malformed_escapes() {
        assert_eq!(decode("100%"), b"100%");
        assert_eq!(decode("%G1%4"), b"%G1%4");
    }

    #[test]
    fn checks_well_formedness() {
        assert!(is_well_formed("a%20b"));
        assert!(is_well_formed("no escapes"));
        assert!(!is_well_formed("100%"));
        assert!(!is_well_formed("%G1"));
        assert!(!is_well_formed("%4"));
    }
}
