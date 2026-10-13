//! Login sequences: steps a terminal session runs once after it connects,
//! waiting for prompts and answering them (`server/src/login-sequence`,
//! `client/src/login-sequence.ts`).

pub mod editor;
pub mod matcher;
pub mod resolve;
pub mod runner;
pub mod session;

pub use matcher::{
    LOGIN_OUTPUT_WINDOW, LoginOutputBuffer, MatchError, OutputMatch, TerminalTextStream,
    WaitPattern, compile_wait_pattern,
};
pub use resolve::{
    HostLoginSequence, LoginSequenceHost, LoginSequenceSource, resolve_login_sequence,
};
pub use runner::{
    AbortSignal, LoginSequenceIo, LoginSequenceOutcome, LoginSequenceProgress, LoginSequenceRunner,
    describe_login_step,
};
pub use session::{
    LoginSequenceRecorder, LoginSequenceSession, LoginSequenceSessionOptions, SecretVault,
    TerminalInputWriter, VaultError, secret_keystrokes,
};
