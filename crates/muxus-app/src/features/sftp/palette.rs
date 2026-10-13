//! The file browser's content colors (not chrome): each file and folder kind's badge
//! color, glyph or label (`FILE_VISUALS` and `FOLDER_VISUALS` of `file-icons.tsx`).

use super::icons::{FileIconKind, FileVisual, FolderIconKind};

/// A badge label's ink unless its kind names one.
pub const LABEL_INK: &str = "#fff";
/// A symbolic link's icon.
pub const LINK_COLOR: &str = "#78909c";

impl FileIconKind {
    /// Badge color, glyph or label (`FILE_VISUALS`).
    pub fn visual(self) -> FileVisual {
        match self {
            Self::Archive => FileVisual::new("#c49a6c", Some("ArchiveRounded"), None, None),
            Self::Audio => FileVisual::new("#66bb6a", Some("AudioFileRounded"), None, None),
            Self::Binary => FileVisual::new("#78909c", None, Some("01"), None),
            Self::Build => FileVisual::new("#ffb74d", None, Some("⚒"), Some("#4a2b00")),
            Self::C => FileVisual::new("#5c6bc0", None, Some("C"), None),
            Self::Certificate => {
                FileVisual::new("#4db6ac", Some("VerifiedUserRounded"), None, None)
            }
            Self::Code => FileVisual::new("#90a4ae", None, Some("<>"), None),
            Self::Config => FileVisual::new("#90a4ae", Some("SettingsRounded"), None, None),
            Self::Cpp => FileVisual::new("#5c6bc0", None, Some("C+"), None),
            Self::Csharp => FileVisual::new("#9b4f96", None, Some("C#"), None),
            Self::Css => FileVisual::new("#42a5f5", None, Some("#"), None),
            Self::Csv => FileVisual::new("#66bb6a", None, Some("CSV"), Some("#143a17")),
            Self::Database => FileVisual::new("#ab47bc", Some("StorageRounded"), None, None),
            Self::Docker => FileVisual::new("#2496ed", None, Some("DK"), None),
            Self::Env => FileVisual::new("#ecd53f", None, Some("ENV"), Some("#3b3400")),
            Self::Font => FileVisual::new("#ab47bc", Some("FontDownloadRounded"), None, None),
            Self::Git => FileVisual::new("#f05032", None, Some("◆"), None),
            Self::Go => FileVisual::new("#00add8", None, Some("GO"), Some("#00313d")),
            Self::Graphql => FileVisual::new("#e535ab", None, Some("GQ"), None),
            Self::Html => FileVisual::new("#e44d26", None, Some("5"), None),
            Self::Image => FileVisual::new("#ba68c8", Some("ImageRounded"), None, None),
            Self::Java => FileVisual::new("#e76f00", None, Some("JV"), None),
            Self::Javascript => FileVisual::new("#f7df1e", None, Some("JS"), Some("#332f00")),
            Self::Json => {
                FileVisual::new("#e9c46a", Some("DataObjectRounded"), None, Some("#443600"))
            }
            Self::Key => FileVisual::new("#f5c451", Some("KeyRounded"), None, None),
            Self::Kotlin => FileVisual::new("#7f52ff", None, Some("KT"), None),
            Self::License => FileVisual::new("#d4a72c", None, Some("§"), Some("#3e2d00")),
            Self::Lock => FileVisual::new("#e0a84b", Some("LockRounded"), None, None),
            Self::Log => FileVisual::new("#9e9e9e", None, Some("LOG"), Some("#292929")),
            Self::Markdown => FileVisual::new("#519aba", None, Some("M↓"), None),
            Self::Npm => FileVisual::new("#cb3837", None, Some("N"), None),
            Self::Pdf => FileVisual::new("#ef5350", Some("PictureAsPdfRounded"), None, None),
            Self::Php => FileVisual::new("#777bb4", None, Some("PHP"), None),
            Self::Pnpm => FileVisual::new("#f69220", None, Some("PN"), Some("#442300")),
            Self::Powershell => FileVisual::new("#2671be", None, Some(">_"), None),
            Self::Proto => FileVisual::new("#f1b44c", None, Some("PB"), Some("#3d2a00")),
            Self::Python => FileVisual::new("#3776ab", None, Some("PY"), None),
            Self::React => FileVisual::new("#61dafb", None, Some("⚛"), Some("#073642")),
            Self::Ruby => FileVisual::new("#cc342d", None, Some("RB"), None),
            Self::Rust => FileVisual::new("#dea584", None, Some("RS"), Some("#3d251b")),
            Self::Sass => FileVisual::new("#cd6799", None, Some("S"), None),
            Self::Shell => FileVisual::new("#66bb6a", Some("TerminalRounded"), None, None),
            Self::Sql => FileVisual::new("#ec6f9e", None, Some("SQL"), Some("#491729")),
            Self::Svelte => FileVisual::new("#ff3e00", None, Some("SV"), None),
            Self::Swift => FileVisual::new("#f05138", None, Some("SW"), None),
            Self::Terraform => FileVisual::new("#7b42bc", None, Some("TF"), None),
            Self::Text => FileVisual::new("#90a4ae", Some("ArticleRounded"), None, None),
            Self::Toml => FileVisual::new("#9c6b53", None, Some("T"), None),
            Self::Typescript => FileVisual::new("#3178c6", None, Some("TS"), None),
            Self::Video => FileVisual::new("#ec6f9e", Some("VideoFileRounded"), None, None),
            Self::Vue => FileVisual::new("#42b883", None, Some("V"), Some("#123a2d")),
            Self::Xml => FileVisual::new("#ef8d47", None, Some("<>"), None),
            Self::Yaml => FileVisual::new("#cb171e", None, Some("YML"), None),
            Self::Yarn => FileVisual::new("#2c8ebb", None, Some("Y"), None),
            Self::File => FileVisual::new("#90a4ae", Some("DescriptionRounded"), None, None),
        }
    }
}

impl FolderIconKind {
    /// Folder color and label (`FOLDER_VISUALS`).
    pub fn visual(self) -> FileVisual {
        match self {
            Self::Assets => FileVisual::new("#ba68c8", None, Some("◇"), None),
            Self::Build => FileVisual::new("#ffb74d", None, Some("□"), Some("#4a2b00")),
            Self::Cloud => FileVisual::new("#4fc3f7", None, Some("☁"), Some("#063745")),
            Self::Config => FileVisual::new("#90a4ae", None, Some("⚙"), Some("#263238")),
            Self::Database => FileVisual::new("#ab47bc", None, Some("DB"), None),
            Self::Dependencies => FileVisual::new("#66bb6a", None, Some("N"), Some("#143a17")),
            Self::Docker => FileVisual::new("#2496ed", None, Some("DK"), None),
            Self::Docs => FileVisual::new("#5c9ded", None, Some("¶"), None),
            Self::Git => FileVisual::new("#f0785a", None, Some("◆"), Some("#4a160a")),
            Self::Public => FileVisual::new("#4dd0e1", None, Some("◎"), Some("#073b42")),
            Self::Scripts => FileVisual::new("#e4bd4b", None, Some(">_"), Some("#3b3100")),
            Self::Secure => FileVisual::new("#e0a84b", None, Some("●"), Some("#4a3000")),
            Self::Source => FileVisual::new("#64b5f6", None, Some("</>"), Some("#0a3554")),
            Self::Tests => FileVisual::new("#4db6ac", None, Some("✓"), Some("#0b3b37")),
            Self::Folder => FileVisual::new("#d7a84d", None, None, None),
        }
    }
}
