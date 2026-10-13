// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a
// copy of the MPL was not distributed with this file, You can obtain one at
// https://mozilla.org/MPL/2.0/. Adapted from noVNC (Copyright The noVNC authors).

//! Client-to-server RFB messages. Port of `RFB.messages` in noVNC's `core/rfb.js` (MPL-2.0).

use flate2::{Compress, Compression, FlushCompress};

use crate::decoders::{
    ENCODING_COPY_RECT, ENCODING_HEXTILE, ENCODING_JPEG, ENCODING_RAW, ENCODING_RRE,
    ENCODING_TIGHT, ENCODING_TIGHT_PNG, ENCODING_ZLIB, ENCODING_ZRLE,
};

pub(crate) const PSEUDO_QUALITY_LEVEL_0: i32 = -32;
pub(crate) const PSEUDO_COMPRESS_LEVEL_0: i32 = -256;
pub(crate) const PSEUDO_DESKTOP_SIZE: i32 = -223;
pub(crate) const PSEUDO_LAST_RECT: i32 = -224;
pub(crate) const PSEUDO_CURSOR: i32 = -239;
pub(crate) const PSEUDO_QEMU_EXTENDED_KEY_EVENT: i32 = -258;
pub(crate) const PSEUDO_QEMU_LED_EVENT: i32 = -261;
pub(crate) const PSEUDO_DESKTOP_NAME: i32 = -307;
pub(crate) const PSEUDO_EXTENDED_DESKTOP_SIZE: i32 = -308;
pub(crate) const PSEUDO_XVP: i32 = -309;
pub(crate) const PSEUDO_FENCE: i32 = -312;
pub(crate) const PSEUDO_CONTINUOUS_UPDATES: i32 = -313;
pub(crate) const PSEUDO_EXTENDED_MOUSE_BUTTONS: i32 = -316;
pub(crate) const PSEUDO_VMWARE_CURSOR: i32 = 0x574d_5664;
pub(crate) const PSEUDO_EXTENDED_CLIPBOARD: i32 = 0xc0a1_e5ce_u32 as i32;

pub(crate) const CLIPBOARD_FORMAT_TEXT: u32 = 1;
pub(crate) const CLIPBOARD_ACTION_CAPS: u32 = 1 << 24;
pub(crate) const CLIPBOARD_ACTION_REQUEST: u32 = 1 << 25;
pub(crate) const CLIPBOARD_ACTION_PEEK: u32 = 1 << 26;
pub(crate) const CLIPBOARD_ACTION_NOTIFY: u32 = 1 << 27;
pub(crate) const CLIPBOARD_ACTION_PROVIDE: u32 = 1 << 28;

/// The encodings to ask for, in preference order (`_sendEncodings`). H.264 is left out: there
/// is no decoder for it here, and servers fall back to Tight.
pub(crate) fn encodings(depth: u8, quality_level: u8, compression_level: u8) -> Vec<i32> {
    let mut list = vec![ENCODING_COPY_RECT];
    // Only supported with full depth support.
    if depth == 24 {
        list.extend([
            ENCODING_TIGHT,
            ENCODING_TIGHT_PNG,
            ENCODING_ZRLE,
            ENCODING_JPEG,
            ENCODING_HEXTILE,
            ENCODING_RRE,
            ENCODING_ZLIB,
        ]);
    }
    list.push(ENCODING_RAW);
    list.extend([
        PSEUDO_QUALITY_LEVEL_0 + i32::from(quality_level),
        PSEUDO_COMPRESS_LEVEL_0 + i32::from(compression_level),
        PSEUDO_DESKTOP_SIZE,
        PSEUDO_LAST_RECT,
        PSEUDO_QEMU_EXTENDED_KEY_EVENT,
        PSEUDO_QEMU_LED_EVENT,
        PSEUDO_EXTENDED_DESKTOP_SIZE,
        PSEUDO_XVP,
        PSEUDO_FENCE,
        PSEUDO_CONTINUOUS_UPDATES,
        PSEUDO_DESKTOP_NAME,
        PSEUDO_EXTENDED_CLIPBOARD,
        PSEUDO_EXTENDED_MOUSE_BUTTONS,
    ]);
    if depth == 24 {
        list.extend([PSEUDO_VMWARE_CURSOR, PSEUDO_CURSOR]);
    }
    list
}

pub(crate) fn set_encodings(encodings: &[i32]) -> Vec<u8> {
    let mut message = vec![2, 0];
    message.extend((encodings.len() as u16).to_be_bytes());
    for encoding in encodings {
        message.extend(encoding.to_be_bytes());
    }
    message
}

/// SetPixelFormat: true colour, little endian, red in the low bits.
pub(crate) fn set_pixel_format(depth: u8) -> Vec<u8> {
    let bpp = if depth > 16 {
        32
    } else if depth > 8 {
        16
    } else {
        8
    };
    let bits = depth / 3;
    let max = ((1u16 << bits) - 1).to_be_bytes();
    let mut message = vec![0, 0, 0, 0, bpp, depth, 0, 1];
    message.extend(max);
    message.extend(max);
    message.extend(max);
    message.extend([0, bits, bits * 2, 0, 0, 0]);
    message
}

pub(crate) fn framebuffer_update_request(
    incremental: bool,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
) -> Vec<u8> {
    let mut message = vec![3, u8::from(incremental)];
    for value in [x, y, width, height] {
        message.extend(value.to_be_bytes());
    }
    message
}

pub(crate) fn key_event(keysym: u32, down: bool) -> Vec<u8> {
    let mut message = vec![4, u8::from(down), 0, 0];
    message.extend(keysym.to_be_bytes());
    message
}

pub(crate) fn qemu_extended_key_event(keysym: u32, down: bool, keycode: u32) -> Vec<u8> {
    let mut message = vec![255, 0];
    message.extend(u16::from(down).to_be_bytes());
    message.extend(keysym.to_be_bytes());
    message.extend(keycode.to_be_bytes());
    message
}

pub(crate) fn pointer_event(x: u16, y: u16, mask: u16) -> Vec<u8> {
    // The marker bit must stay 0, or the server takes it for the extended format.
    let mut message = vec![5, (mask & 0x7f) as u8];
    message.extend(x.to_be_bytes());
    message.extend(y.to_be_bytes());
    message
}

pub(crate) fn extended_pointer_event(x: u16, y: u16, mask: u16) -> Vec<u8> {
    let higher = ((mask >> 7) & 0xff) as u8;
    let lower = (mask & 0x7f) as u8 | 0x80;
    let mut message = vec![5, lower];
    message.extend(x.to_be_bytes());
    message.extend(y.to_be_bytes());
    message.push(higher);
    message
}

pub(crate) fn client_cut_text(data: &[u8], extended: bool) -> Vec<u8> {
    let mut message = vec![6, 0, 0, 0];
    let length = if extended {
        (-(data.len() as i32)) as u32
    } else {
        data.len() as u32
    };
    message.extend(length.to_be_bytes());
    message.extend(data);
    message
}

/// ISO 8859-1 text for a classic ClientCutText (other characters become `?`).
pub(crate) fn latin1(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
        .collect()
}

fn clipboard_flags(actions: u32, formats: u32) -> [u8; 4] {
    [(actions >> 24) as u8, 0, 0, formats as u8]
}

pub(crate) fn extended_clipboard_caps() -> Vec<u8> {
    let actions = CLIPBOARD_ACTION_CAPS
        | CLIPBOARD_ACTION_REQUEST
        | CLIPBOARD_ACTION_PEEK
        | CLIPBOARD_ACTION_NOTIFY
        | CLIPBOARD_ACTION_PROVIDE;
    let mut data = clipboard_flags(actions, CLIPBOARD_FORMAT_TEXT).to_vec();
    // We don't send unsolicited clipboard data, so the size limit is 0.
    data.extend(0u32.to_be_bytes());
    client_cut_text(&data, true)
}

pub(crate) fn extended_clipboard_notify(formats: u32) -> Vec<u8> {
    client_cut_text(&clipboard_flags(CLIPBOARD_ACTION_NOTIFY, formats), true)
}

pub(crate) fn extended_clipboard_request(formats: u32) -> Vec<u8> {
    client_cut_text(&clipboard_flags(CLIPBOARD_ACTION_REQUEST, formats), true)
}

/// Text in an extended Provide: CRLF line ends, NUL-terminated UTF-8, length-prefixed, deflated.
pub(crate) fn extended_clipboard_provide(text: &str) -> Vec<u8> {
    let normalized = text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', "\r\n");
    let mut plain = Vec::new();
    let bytes = [normalized.as_bytes(), b"\0"].concat();
    plain.extend((bytes.len() as u32).to_be_bytes());
    plain.extend(bytes);
    let mut compress = Compress::new(Compression::default(), true);
    let mut deflated = Vec::with_capacity(plain.len() + 64);
    loop {
        let consumed = compress.total_in() as usize;
        if deflated.len() == deflated.capacity() {
            deflated.reserve(4096);
        }
        compress
            .compress_vec(&plain[consumed..], &mut deflated, FlushCompress::Full)
            .expect("deflate cannot fail on memory input");
        if compress.total_in() as usize >= plain.len() && deflated.len() < deflated.capacity() {
            break;
        }
    }
    let mut data = clipboard_flags(CLIPBOARD_ACTION_PROVIDE, CLIPBOARD_FORMAT_TEXT).to_vec();
    data.extend(deflated);
    client_cut_text(&data, true)
}

pub(crate) fn set_desktop_size(width: u16, height: u16, screen_id: u32, flags: u32) -> Vec<u8> {
    let mut message = vec![251, 0];
    message.extend(width.to_be_bytes());
    message.extend(height.to_be_bytes());
    message.extend([1, 0]);
    message.extend(screen_id.to_be_bytes());
    message.extend(0u16.to_be_bytes());
    message.extend(0u16.to_be_bytes());
    message.extend(width.to_be_bytes());
    message.extend(height.to_be_bytes());
    message.extend(flags.to_be_bytes());
    message
}

pub(crate) fn client_fence(flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut message = vec![248, 0, 0, 0];
    message.extend(flags.to_be_bytes());
    message.push(payload.len() as u8);
    message.extend(payload);
    message
}

pub(crate) fn enable_continuous_updates(
    enable: bool,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
) -> Vec<u8> {
    let mut message = vec![150, u8::from(enable)];
    for value in [x, y, width, height] {
        message.extend(value.to_be_bytes());
    }
    message
}

pub(crate) fn xvp_operation(version: u8, operation: u8) -> Vec<u8> {
    vec![250, 0, version, operation]
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Decompress, FlushDecompress};

    #[test]
    fn encodes_pixel_format_for_depth_24() {
        assert_eq!(
            set_pixel_format(24),
            [
                0, 0, 0, 0, 32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 0, 8, 16, 0, 0, 0
            ]
        );
        // Intel AMT KVM's low colour mode: 2 bits per channel.
        assert_eq!(
            &set_pixel_format(8)[4..17],
            &[8, 8, 0, 1, 0, 3, 0, 3, 0, 3, 0, 2, 4]
        );
    }

    #[test]
    fn asks_for_encodings_in_novncs_order() {
        let list = encodings(24, 6, 2);
        assert_eq!(&list[..3], &[1, 7, -260]);
        assert!(list.contains(&-26) && list.contains(&-254));
        assert_eq!(list.last(), Some(&-239));
        assert_eq!(encodings(8, 6, 2)[..2], [1, 0]);
        let message = set_encodings(&[0, -223]);
        assert_eq!(message, [2, 0, 0, 2, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0x21]);
    }

    #[test]
    fn encodes_input_messages() {
        assert_eq!(key_event(0xff0d, true), [4, 1, 0, 0, 0, 0, 0xff, 0x0d]);
        assert_eq!(
            qemu_extended_key_event(0x61, false, 0x1e),
            [255, 0, 0, 0, 0, 0, 0, 0x61, 0, 0, 0, 0x1e]
        );
        assert_eq!(pointer_event(1, 2, 0x81), [5, 1, 0, 1, 0, 2]);
        assert_eq!(
            extended_pointer_event(1, 2, 0x181),
            [5, 0x81, 0, 1, 0, 2, 3]
        );
    }

    #[test]
    fn encodes_clipboard_messages() {
        assert_eq!(
            client_cut_text(b"hi", false),
            [6, 0, 0, 0, 0, 0, 0, 2, b'h', b'i']
        );
        assert_eq!(latin1("añ€"), [b'a', 0xf1, b'?']);
        assert_eq!(
            extended_clipboard_notify(1),
            [6, 0, 0, 0, 0xff, 0xff, 0xff, 0xfc, 0x08, 0, 0, 1]
        );
        assert_eq!(
            extended_clipboard_caps(),
            [
                6, 0, 0, 0, 0xff, 0xff, 0xff, 0xf8, 0x1f, 0, 0, 1, 0, 0, 0, 0
            ]
        );
        let provide = extended_clipboard_provide("a\nb");
        assert_eq!(&provide[8..12], &[0x10, 0, 0, 1]);
        let mut inflate = Decompress::new(true);
        let mut plain = Vec::with_capacity(64);
        inflate
            .decompress_vec(&provide[12..], &mut plain, FlushDecompress::Sync)
            .unwrap();
        assert_eq!(plain, [0, 0, 0, 5, b'a', b'\r', b'\n', b'b', 0]);
    }

    #[test]
    fn encodes_desktop_size_and_fence() {
        let message = set_desktop_size(1024, 768, 7, 0);
        assert_eq!(message.len(), 24);
        assert_eq!(&message[..8], &[251, 0, 4, 0, 3, 0, 1, 0]);
        assert_eq!(
            client_fence(3, b"xy"),
            [248, 0, 0, 0, 0, 0, 0, 3, 2, b'x', b'y']
        );
    }
}
