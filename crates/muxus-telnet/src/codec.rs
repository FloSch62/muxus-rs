//! Incremental Telnet codec (RFC 854 and the options Muxus negotiates).

pub const IAC: u8 = 255;
pub const DONT: u8 = 254;
pub const DO: u8 = 253;
pub const WONT: u8 = 252;
pub const WILL: u8 = 251;
pub const SB: u8 = 250;
pub const BRK: u8 = 243;
pub const SE: u8 = 240;

pub const OPT_BINARY: u8 = 0;
pub const OPT_ECHO: u8 = 1;
pub const OPT_SUPPRESS_GO_AHEAD: u8 = 3;
pub const OPT_TERMINAL_TYPE: u8 = 24;
pub const OPT_NAWS: u8 = 31;

const TERMINAL_TYPE_IS: u8 = 0;
const TERMINAL_TYPE_SEND: u8 = 1;
/// The terminal type announced in TERMINAL-TYPE sub-negotiation.
pub const DEFAULT_TERMINAL_TYPE: &str = "xterm-256color";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParserState {
    Data,
    Iac,
    Option,
    SbOption,
    SbData,
    SbIac,
}

/// What one call to [`TelnetCodec::feed`] produced.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Fed {
    /// Application bytes for the terminal.
    pub terminal: Vec<u8>,
    /// Negotiation replies for the server, one entry per command.
    pub replies: Vec<Vec<u8>>,
}

/// Incremental Telnet codec. Negotiation bytes are consumed and answered,
/// while application bytes are emitted for the terminal. State deliberately
/// spans TCP chunks because IAC commands and sub-negotiations can be
/// fragmented at any byte boundary.
#[derive(Debug, Clone)]
pub struct TelnetCodec {
    state: ParserState,
    command: u8,
    sub_option: u8,
    sub_data: Vec<u8>,
    remote_enabled: [bool; 256],
    local_enabled: [bool; 256],
    pending_cr: bool,
    cols: u16,
    rows: u16,
}

impl TelnetCodec {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            state: ParserState::Data,
            command: 0,
            sub_option: 0,
            sub_data: Vec::new(),
            remote_enabled: [false; 256],
            local_enabled: [false; 256],
            pending_cr: false,
            cols,
            rows,
        }
    }

    /// Consume bytes from the server.
    pub fn feed(&mut self, chunk: &[u8]) -> Fed {
        let mut fed = Fed::default();
        for &byte in chunk {
            match self.state {
                ParserState::Data => {
                    if byte == IAC {
                        self.state = ParserState::Iac;
                    } else {
                        self.push_application_byte(byte, &mut fed.terminal);
                    }
                }
                ParserState::Iac => {
                    if byte == IAC {
                        self.push_application_byte(IAC, &mut fed.terminal);
                        self.state = ParserState::Data;
                    } else if matches!(byte, WILL | WONT | DO | DONT) {
                        self.command = byte;
                        self.state = ParserState::Option;
                    } else if byte == SB {
                        self.state = ParserState::SbOption;
                    } else {
                        // NOP, GA, AYT, BRK and other two-byte commands carry no terminal data.
                        self.state = ParserState::Data;
                    }
                }
                ParserState::Option => {
                    self.negotiate(self.command, byte, &mut fed.replies);
                    self.state = ParserState::Data;
                }
                ParserState::SbOption => {
                    self.sub_option = byte;
                    self.sub_data.clear();
                    self.state = ParserState::SbData;
                }
                ParserState::SbData => {
                    if byte == IAC {
                        self.state = ParserState::SbIac;
                    } else {
                        self.sub_data.push(byte);
                    }
                }
                ParserState::SbIac => {
                    if byte == IAC {
                        self.sub_data.push(IAC);
                        self.state = ParserState::SbData;
                    } else if byte == SE {
                        let data = std::mem::take(&mut self.sub_data);
                        self.subnegotiate(self.sub_option, &data, &mut fed.replies);
                        self.state = ParserState::Data;
                    } else {
                        // Malformed sub-negotiation: discard it and resynchronize.
                        self.state = ParserState::Data;
                    }
                }
            }
        }
        fed
    }

    /// Encode terminal input for the wire: IAC doubled and, until BINARY is
    /// on, a lone CR or LF turned into the NVT CR LF.
    pub fn encode(&self, data: &[u8]) -> Vec<u8> {
        let mut output = Vec::with_capacity(data.len() + 8);
        let binary = self.local_enabled[OPT_BINARY as usize];
        for (index, &byte) in data.iter().enumerate() {
            if !binary && byte == b'\r' {
                push_escaped(b'\r', &mut output);
                if data.get(index + 1) != Some(&b'\n') {
                    push_escaped(b'\n', &mut output);
                }
                continue;
            }
            if !binary && byte == b'\n' && (index == 0 || data[index - 1] != b'\r') {
                push_escaped(b'\r', &mut output);
            }
            push_escaped(byte, &mut output);
        }
        output
    }

    /// New window size; returns the NAWS update to send, if NAWS is on.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Option<Vec<u8>> {
        self.cols = cols;
        self.rows = rows;
        self.local_enabled[OPT_NAWS as usize].then(|| self.window_size())
    }

    /// The connection closed: a CR held back to see whether NUL follows is
    /// terminal data after all.
    pub fn flush(&mut self) -> Option<Vec<u8>> {
        if !self.pending_cr {
            return None;
        }
        self.pending_cr = false;
        Some(vec![b'\r'])
    }

    fn negotiate(&mut self, command: u8, option: u8, replies: &mut Vec<Vec<u8>>) {
        let index = option as usize;
        match command {
            WILL => {
                let supported = matches!(option, OPT_BINARY | OPT_ECHO | OPT_SUPPRESS_GO_AHEAD);
                if supported {
                    if !self.remote_enabled[index] {
                        self.remote_enabled[index] = true;
                        replies.push(vec![IAC, DO, option]);
                    }
                } else {
                    replies.push(vec![IAC, DONT, option]);
                }
            }
            WONT => {
                if std::mem::take(&mut self.remote_enabled[index]) {
                    replies.push(vec![IAC, DONT, option]);
                }
            }
            DO => {
                let supported = matches!(
                    option,
                    OPT_BINARY | OPT_SUPPRESS_GO_AHEAD | OPT_TERMINAL_TYPE | OPT_NAWS
                );
                if supported {
                    if !self.local_enabled[index] {
                        self.local_enabled[index] = true;
                        replies.push(vec![IAC, WILL, option]);
                    }
                    if option == OPT_NAWS {
                        replies.push(self.window_size());
                    }
                } else {
                    replies.push(vec![IAC, WONT, option]);
                }
            }
            _ => {
                if std::mem::take(&mut self.local_enabled[index]) {
                    replies.push(vec![IAC, WONT, option]);
                }
            }
        }
    }

    fn subnegotiate(&self, option: u8, data: &[u8], replies: &mut Vec<Vec<u8>>) {
        if option != OPT_TERMINAL_TYPE
            || !self.local_enabled[OPT_TERMINAL_TYPE as usize]
            || data.first() != Some(&TERMINAL_TYPE_SEND)
        {
            return;
        }
        let mut payload = vec![TERMINAL_TYPE_IS];
        payload.extend_from_slice(DEFAULT_TERMINAL_TYPE.as_bytes());
        replies.push(subnegotiation(OPT_TERMINAL_TYPE, &payload));
    }

    fn window_size(&self) -> Vec<u8> {
        let cols = self.cols.max(1);
        let rows = self.rows.max(1);
        let mut size = Vec::with_capacity(4);
        size.extend_from_slice(&cols.to_be_bytes());
        size.extend_from_slice(&rows.to_be_bytes());
        subnegotiation(OPT_NAWS, &size)
    }

    fn push_application_byte(&mut self, byte: u8, output: &mut Vec<u8>) {
        if self.remote_enabled[OPT_BINARY as usize] {
            if self.pending_cr {
                output.push(b'\r');
                self.pending_cr = false;
            }
            output.push(byte);
            return;
        }
        if self.pending_cr {
            output.push(b'\r');
            self.pending_cr = false;
            if byte == 0 {
                return;
            }
        }
        if byte == b'\r' {
            self.pending_cr = true;
        } else {
            output.push(byte);
        }
    }
}

fn push_escaped(byte: u8, output: &mut Vec<u8>) {
    output.push(byte);
    if byte == IAC {
        output.push(IAC);
    }
}

fn subnegotiation(option: u8, data: &[u8]) -> Vec<u8> {
    let mut frame = vec![IAC, SB, option];
    for &byte in data {
        push_escaped(byte, &mut frame);
    }
    frame.extend_from_slice(&[IAC, SE]);
    frame
}
