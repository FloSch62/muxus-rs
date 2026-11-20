//! Decoding of imported text files (port of `shared/src/text-decoding.ts`).

/// What the bytes 0x80 to 0x9F are in Windows-1252 (the WHATWG index, which
/// keeps the five unassigned bytes as the C1 controls of the same number).
const WINDOWS_1252_HIGH: [char; 32] = [
    '\u{20ac}', '\u{81}', '\u{201a}', '\u{192}', '\u{201e}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{2c6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\u{8d}', '\u{17d}', '\u{8f}',
    '\u{90}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{2dc}', '\u{2122}', '\u{161}', '\u{203a}', '\u{153}', '\u{9d}', '\u{17e}', '\u{178}',
];

/// Decode an imported text file whose encoding is not declared.
///
/// Windows tools such as MobaXterm save their INI files in the ANSI code page,
/// so a name like "Sessions » Lab" arrives as the lone byte 0xBB. Reading that
/// as UTF-8 turns it into U+FFFD. Strict UTF-8 is tried first (it also covers
/// plain ASCII), and Windows-1252 is the fallback when the bytes are not valid
/// UTF-8. A leading UTF-8 byte order mark is dropped from UTF-8 text.
pub fn decode_imported_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.strip_prefix('\u{feff}').unwrap_or(text).to_owned(),
        Err(_) => decode_windows_1252(bytes),
    }
}

/// Every byte is one character in Windows-1252, so this cannot fail.
pub fn decode_windows_1252(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&byte| match byte {
            0x80..=0x9f => WINDOWS_1252_HIGH[usize::from(byte - 0x80)],
            _ => char::from(byte),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_utf8_text_unchanged() {
        let text = "Sessions » Lab Antwerp é ü °";
        assert_eq!(decode_imported_text(text.as_bytes()), text);
    }

    #[test]
    fn drops_a_utf8_byte_order_mark() {
        let mut bytes = vec![0xef, 0xbb, 0xbf];
        bytes.extend_from_slice(b"[Bookmarks]");
        assert_eq!(decode_imported_text(&bytes), "[Bookmarks]");
    }

    #[test]
    fn falls_back_to_windows_1252_for_ansi_files() {
        // "Sessions » PoCs é €" as written by MobaXterm in the Windows ANSI code page.
        let mut bytes = b"Sessions ".to_vec();
        bytes.push(0xbb);
        bytes.extend_from_slice(b" PoCs ");
        bytes.extend_from_slice(&[0xe9, 0x20, 0x80]);
        assert_eq!(decode_imported_text(&bytes), "Sessions » PoCs é €");
    }

    #[test]
    fn maps_every_windows_1252_byte() {
        let all: Vec<u8> = (0..=255).collect();
        let decoded: Vec<char> = decode_windows_1252(&all).chars().collect();
        assert_eq!(decoded.len(), 256);
        assert_eq!(decoded[0x41], 'A');
        assert_eq!(decoded[0x80], '€');
        assert_eq!(decoded[0x81], '\u{81}');
        assert_eq!(decoded[0x99], '™');
        assert_eq!(decoded[0x9f], 'Ÿ');
        assert_eq!(decoded[0xa0], '\u{a0}');
        assert_eq!(decoded[0xff], 'ÿ');
    }

    #[test]
    fn keeps_a_byte_order_mark_that_starts_an_ansi_file() {
        // Only the UTF-8 decoder knows the mark; an ANSI file keeps its bytes.
        assert_eq!(decode_imported_text(b"\xEF\xBB\xBFa\xFF"), "ï»¿aÿ");
    }
}
