//! Terminal sessions as the window drives them: the link to a session
//! ([`link`]), how an ended or lost session is presented ([`recovery`]) and
//! the diagnosis it offers ([`diagnostics`]).

pub mod diagnostics;
pub mod link;
pub mod recovery;

pub use link::{BoxFuture, LinkEnds, LinkEvent, LinkInput, LinkReader, LinkWriter, SessionLink};
