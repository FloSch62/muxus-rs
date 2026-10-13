//! Text clipboard redirection over CLIPRDR.
//!
//! The TypeScript client only ever moves `text/plain` between the local and the remote
//! clipboard, so this backend speaks `CF_UNICODETEXT` alone. CLIPRDR calls the backend from
//! inside the session's PDU processing; the backend only forwards what happened to the session
//! loop, which owns the channel and answers there.

use ironrdp::cliprdr::backend::CliprdrBackend;
use ironrdp::cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FileContentsRequest,
    FileContentsResponse, FormatDataRequest, FormatDataResponse, LockDataId,
    OwnedFormatDataResponse,
};
use ironrdp::core::IntoOwned as _;
use tokio::sync::mpsc;

/// What the server did on the clipboard channel.
#[derive(Debug)]
pub(crate) enum ClipboardSignal {
    /// The remote clipboard changed and offers these formats.
    RemoteFormats(Vec<ClipboardFormat>),
    /// The server pastes and wants our clipboard in this format.
    DataRequest(ClipboardFormatId),
    /// The data we asked for (or an error response).
    DataResponse(OwnedFormatDataResponse),
    /// The server asks for our format list (start of the channel): offer what we hold.
    ForceUpdate,
}

#[derive(Debug)]
pub(crate) struct TextClipboardBackend {
    signals: mpsc::UnboundedSender<ClipboardSignal>,
}

impl TextClipboardBackend {
    pub(crate) fn new(signals: mpsc::UnboundedSender<ClipboardSignal>) -> Self {
        Self { signals }
    }

    fn signal(&self, signal: ClipboardSignal) {
        // The session is going away; its run loop reports why.
        let _ = self.signals.send(signal);
    }
}

ironrdp::core::impl_as_any!(TextClipboardBackend);

impl CliprdrBackend for TextClipboardBackend {
    fn temporary_directory(&self) -> &str {
        ".cliprdr"
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        // Text only: no file streams, no clipboard locks.
        ClipboardGeneralCapabilityFlags::empty()
    }

    fn on_ready(&mut self) {}

    fn on_request_format_list(&mut self) {
        self.signal(ClipboardSignal::ForceUpdate);
    }

    fn on_process_negotiated_capabilities(
        &mut self,
        _capabilities: ClipboardGeneralCapabilityFlags,
    ) {
    }

    fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
        self.signal(ClipboardSignal::RemoteFormats(available_formats.to_vec()));
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        self.signal(ClipboardSignal::DataRequest(request.format));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        self.signal(ClipboardSignal::DataResponse(response.into_owned()));
    }

    fn on_file_contents_request(&mut self, _request: FileContentsRequest) {}

    fn on_file_contents_response(&mut self, _response: FileContentsResponse<'_>) {}

    fn on_lock(&mut self, _data_id: LockDataId) {}

    fn on_unlock(&mut self, _data_id: LockDataId) {}
}

/// Whether a remote format list carries text we can paste.
pub(crate) fn offers_text(formats: &[ClipboardFormat]) -> bool {
    formats
        .iter()
        .any(|format| format.id() == ClipboardFormatId::CF_UNICODETEXT)
}

/// The text of a `CF_UNICODETEXT` response, without the terminating NUL.
pub(crate) fn response_text(response: &OwnedFormatDataResponse) -> Option<String> {
    if response.is_error() {
        return None;
    }
    let text = response.to_unicode_string().ok()?;
    Some(text.trim_end_matches('\0').to_owned())
}
