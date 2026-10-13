//! Icons for remote files and folders: the name resolver of
//! `client/src/file-icon-resolver.ts` and the badges of
//! `client/src/components/FileTypeIcon.tsx` (vscode-icons-inspired: a colored
//! glyph, or a document with a short label).

use std::cell::RefCell;
use std::collections::HashMap;

use muxus_core::api::SftpEntryType;
use muxus_ui::color::hex;
use muxus_ui::gpui_kit::{AnyElement, Hsla, IntoElement, ParentElement, Styled, div, px};
use muxus_ui::icon::icon;
use muxus_ui::label::label;

/// Badge color, glyph or label of one kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileVisual {
    pub color: &'static str,
    /// A Material icon drawn in `color` instead of the labelled document.
    pub glyph: Option<&'static str>,
    pub label: Option<&'static str>,
    pub label_color: Option<&'static str>,
}

impl FileVisual {
    pub(super) const fn new(
        color: &'static str,
        glyph: Option<&'static str>,
        label: Option<&'static str>,
        label_color: Option<&'static str>,
    ) -> Self {
        Self {
            color,
            glyph,
            label,
            label_color,
        }
    }
}

/// The look of a remote file, keyed off its name (`FileIconKind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileIconKind {
    Archive,
    Audio,
    Binary,
    Build,
    C,
    Certificate,
    Code,
    Config,
    Cpp,
    Csharp,
    Css,
    Csv,
    Database,
    Docker,
    Env,
    Font,
    Git,
    Go,
    Graphql,
    Html,
    Image,
    Java,
    Javascript,
    Json,
    Key,
    Kotlin,
    License,
    Lock,
    Log,
    Markdown,
    Npm,
    Pdf,
    Php,
    Pnpm,
    Powershell,
    Proto,
    Python,
    React,
    Ruby,
    Rust,
    Sass,
    Shell,
    Sql,
    Svelte,
    Swift,
    Terraform,
    Text,
    Toml,
    Typescript,
    Video,
    Vue,
    Xml,
    Yaml,
    Yarn,
    File,
}

/// The look of a remote folder (`FolderIconKind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FolderIconKind {
    Assets,
    Build,
    Cloud,
    Config,
    Database,
    Dependencies,
    Docker,
    Docs,
    Git,
    Public,
    Scripts,
    Secure,
    Source,
    Tests,
    Folder,
}

impl FileIconKind {
    /// The TypeScript name of the kind.
    pub fn name(self) -> &'static str {
        match self {
            Self::Archive => "archive",
            Self::Audio => "audio",
            Self::Binary => "binary",
            Self::Build => "build",
            Self::C => "c",
            Self::Certificate => "certificate",
            Self::Code => "code",
            Self::Config => "config",
            Self::Cpp => "cpp",
            Self::Csharp => "csharp",
            Self::Css => "css",
            Self::Csv => "csv",
            Self::Database => "database",
            Self::Docker => "docker",
            Self::Env => "env",
            Self::Font => "font",
            Self::Git => "git",
            Self::Go => "go",
            Self::Graphql => "graphql",
            Self::Html => "html",
            Self::Image => "image",
            Self::Java => "java",
            Self::Javascript => "javascript",
            Self::Json => "json",
            Self::Key => "key",
            Self::Kotlin => "kotlin",
            Self::License => "license",
            Self::Lock => "lock",
            Self::Log => "log",
            Self::Markdown => "markdown",
            Self::Npm => "npm",
            Self::Pdf => "pdf",
            Self::Php => "php",
            Self::Pnpm => "pnpm",
            Self::Powershell => "powershell",
            Self::Proto => "proto",
            Self::Python => "python",
            Self::React => "react",
            Self::Ruby => "ruby",
            Self::Rust => "rust",
            Self::Sass => "sass",
            Self::Shell => "shell",
            Self::Sql => "sql",
            Self::Svelte => "svelte",
            Self::Swift => "swift",
            Self::Terraform => "terraform",
            Self::Text => "text",
            Self::Toml => "toml",
            Self::Typescript => "typescript",
            Self::Video => "video",
            Self::Vue => "vue",
            Self::Xml => "xml",
            Self::Yaml => "yaml",
            Self::Yarn => "yarn",
            Self::File => "file",
        }
    }
}

impl FolderIconKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Assets => "assets",
            Self::Build => "build",
            Self::Cloud => "cloud",
            Self::Config => "config",
            Self::Database => "database",
            Self::Dependencies => "dependencies",
            Self::Docker => "docker",
            Self::Docs => "docs",
            Self::Git => "git",
            Self::Public => "public",
            Self::Scripts => "scripts",
            Self::Secure => "secure",
            Self::Source => "source",
            Self::Tests => "tests",
            Self::Folder => "folder",
        }
    }
}

const EXACT_FILE_ICONS: &[(&str, FileIconKind)] = &[
    (".babelrc", FileIconKind::Javascript),
    (".dockerignore", FileIconKind::Docker),
    (".editorconfig", FileIconKind::Config),
    (".eslintignore", FileIconKind::Config),
    (".eslintrc", FileIconKind::Javascript),
    (".gitattributes", FileIconKind::Git),
    (".gitconfig", FileIconKind::Git),
    (".gitignore", FileIconKind::Git),
    (".gitmodules", FileIconKind::Git),
    (".npmrc", FileIconKind::Npm),
    (".nvmrc", FileIconKind::Javascript),
    (".prettierignore", FileIconKind::Config),
    (".prettierrc", FileIconKind::Config),
    (".profile", FileIconKind::Shell),
    (".python-version", FileIconKind::Python),
    (".ruby-version", FileIconKind::Ruby),
    (".tool-versions", FileIconKind::Config),
    (".yarnrc", FileIconKind::Yarn),
    (".zshrc", FileIconKind::Shell),
    ("bun.lock", FileIconKind::Lock),
    ("bun.lockb", FileIconKind::Lock),
    ("cargo.lock", FileIconKind::Lock),
    ("cargo.toml", FileIconKind::Rust),
    ("composer.json", FileIconKind::Php),
    ("composer.lock", FileIconKind::Lock),
    ("containerfile", FileIconKind::Docker),
    ("copying", FileIconKind::License),
    ("dockerfile", FileIconKind::Docker),
    ("gemfile", FileIconKind::Ruby),
    ("gemfile.lock", FileIconKind::Lock),
    ("go.mod", FileIconKind::Go),
    ("go.sum", FileIconKind::Lock),
    ("gradle.properties", FileIconKind::Java),
    ("gradlew", FileIconKind::Build),
    ("gradlew.bat", FileIconKind::Build),
    ("justfile", FileIconKind::Build),
    ("license", FileIconKind::License),
    ("makefile", FileIconKind::Build),
    ("package-lock.json", FileIconKind::Npm),
    ("package.json", FileIconKind::Npm),
    ("pnpm-lock.yaml", FileIconKind::Pnpm),
    ("pnpm-workspace.yaml", FileIconKind::Pnpm),
    ("poetry.lock", FileIconKind::Lock),
    ("pyproject.toml", FileIconKind::Python),
    ("readme", FileIconKind::Markdown),
    ("requirements.txt", FileIconKind::Python),
    ("tsconfig.json", FileIconKind::Typescript),
    ("vite.config.js", FileIconKind::Javascript),
    ("vite.config.mjs", FileIconKind::Javascript),
    ("vite.config.ts", FileIconKind::Typescript),
    ("webpack.config.js", FileIconKind::Javascript),
    ("webpack.config.ts", FileIconKind::Typescript),
    ("yarn.lock", FileIconKind::Yarn),
];

const EXTENSION_FILE_ICONS: &[(&str, FileIconKind)] = &[
    ("7z", FileIconKind::Archive),
    ("aac", FileIconKind::Audio),
    ("apk", FileIconKind::Binary),
    ("avi", FileIconKind::Video),
    ("bat", FileIconKind::Powershell),
    ("bin", FileIconKind::Binary),
    ("bmp", FileIconKind::Image),
    ("bz2", FileIconKind::Archive),
    ("c", FileIconKind::C),
    ("cc", FileIconKind::Cpp),
    ("cer", FileIconKind::Certificate),
    ("cert", FileIconKind::Certificate),
    ("cjs", FileIconKind::Javascript),
    ("class", FileIconKind::Binary),
    ("cmd", FileIconKind::Powershell),
    ("conf", FileIconKind::Config),
    ("cpp", FileIconKind::Cpp),
    ("crt", FileIconKind::Certificate),
    ("cs", FileIconKind::Csharp),
    ("css", FileIconKind::Css),
    ("csv", FileIconKind::Csv),
    ("cts", FileIconKind::Typescript),
    ("cxx", FileIconKind::Cpp),
    ("dart", FileIconKind::Code),
    ("db", FileIconKind::Database),
    ("deb", FileIconKind::Binary),
    ("der", FileIconKind::Certificate),
    ("dll", FileIconKind::Binary),
    ("dmg", FileIconKind::Binary),
    ("doc", FileIconKind::Text),
    ("docx", FileIconKind::Text),
    ("eot", FileIconKind::Font),
    ("env", FileIconKind::Env),
    ("exe", FileIconKind::Binary),
    ("flac", FileIconKind::Audio),
    ("gif", FileIconKind::Image),
    ("go", FileIconKind::Go),
    ("gql", FileIconKind::Graphql),
    ("graphql", FileIconKind::Graphql),
    ("gz", FileIconKind::Archive),
    ("h", FileIconKind::C),
    ("hpp", FileIconKind::Cpp),
    ("htm", FileIconKind::Html),
    ("html", FileIconKind::Html),
    ("ico", FileIconKind::Image),
    ("ini", FileIconKind::Config),
    ("jar", FileIconKind::Archive),
    ("java", FileIconKind::Java),
    ("jpeg", FileIconKind::Image),
    ("jpg", FileIconKind::Image),
    ("js", FileIconKind::Javascript),
    ("json", FileIconKind::Json),
    ("jsonc", FileIconKind::Json),
    ("jsx", FileIconKind::React),
    ("key", FileIconKind::Key),
    ("kt", FileIconKind::Kotlin),
    ("kts", FileIconKind::Kotlin),
    ("less", FileIconKind::Css),
    ("lock", FileIconKind::Lock),
    ("log", FileIconKind::Log),
    ("lua", FileIconKind::Code),
    ("m4a", FileIconKind::Audio),
    ("markdown", FileIconKind::Markdown),
    ("md", FileIconKind::Markdown),
    ("mdx", FileIconKind::React),
    ("mjs", FileIconKind::Javascript),
    ("mkv", FileIconKind::Video),
    ("mov", FileIconKind::Video),
    ("mp3", FileIconKind::Audio),
    ("mp4", FileIconKind::Video),
    ("mpeg", FileIconKind::Video),
    ("mpg", FileIconKind::Video),
    ("mts", FileIconKind::Typescript),
    ("odt", FileIconKind::Text),
    ("ogg", FileIconKind::Audio),
    ("otf", FileIconKind::Font),
    ("pdf", FileIconKind::Pdf),
    ("pem", FileIconKind::Certificate),
    ("php", FileIconKind::Php),
    ("png", FileIconKind::Image),
    ("ppt", FileIconKind::Text),
    ("pptx", FileIconKind::Text),
    ("proto", FileIconKind::Proto),
    ("ps1", FileIconKind::Powershell),
    ("psd1", FileIconKind::Powershell),
    ("psm1", FileIconKind::Powershell),
    ("py", FileIconKind::Python),
    ("pyc", FileIconKind::Binary),
    ("rar", FileIconKind::Archive),
    ("rb", FileIconKind::Ruby),
    ("rpm", FileIconKind::Binary),
    ("rs", FileIconKind::Rust),
    ("sass", FileIconKind::Sass),
    ("scss", FileIconKind::Sass),
    ("sh", FileIconKind::Shell),
    ("so", FileIconKind::Binary),
    ("sql", FileIconKind::Sql),
    ("sqlite", FileIconKind::Database),
    ("sqlite3", FileIconKind::Database),
    ("svg", FileIconKind::Image),
    ("swift", FileIconKind::Swift),
    ("tar", FileIconKind::Archive),
    ("tf", FileIconKind::Terraform),
    ("tfstate", FileIconKind::Terraform),
    ("tfvars", FileIconKind::Terraform),
    ("tgz", FileIconKind::Archive),
    ("toml", FileIconKind::Toml),
    ("ts", FileIconKind::Typescript),
    ("tsx", FileIconKind::React),
    ("ttf", FileIconKind::Font),
    ("txt", FileIconKind::Text),
    ("vue", FileIconKind::Vue),
    ("wav", FileIconKind::Audio),
    ("webm", FileIconKind::Video),
    ("webp", FileIconKind::Image),
    ("woff", FileIconKind::Font),
    ("woff2", FileIconKind::Font),
    ("xls", FileIconKind::Csv),
    ("xlsx", FileIconKind::Csv),
    ("xml", FileIconKind::Xml),
    ("xz", FileIconKind::Archive),
    ("yaml", FileIconKind::Yaml),
    ("yml", FileIconKind::Yaml),
    ("zip", FileIconKind::Archive),
];

const FOLDER_ICONS: &[(&str, FolderIconKind)] = &[
    (".aws", FolderIconKind::Cloud),
    (".config", FolderIconKind::Config),
    (".devcontainer", FolderIconKind::Docker),
    (".git", FolderIconKind::Git),
    (".github", FolderIconKind::Git),
    (".gitlab", FolderIconKind::Git),
    (".kube", FolderIconKind::Cloud),
    (".ssh", FolderIconKind::Secure),
    (".vscode", FolderIconKind::Config),
    ("__tests__", FolderIconKind::Tests),
    ("api", FolderIconKind::Source),
    ("app", FolderIconKind::Source),
    ("assets", FolderIconKind::Assets),
    ("bin", FolderIconKind::Scripts),
    ("build", FolderIconKind::Build),
    ("config", FolderIconKind::Config),
    ("coverage", FolderIconKind::Tests),
    ("database", FolderIconKind::Database),
    ("db", FolderIconKind::Database),
    ("dist", FolderIconKind::Build),
    ("docker", FolderIconKind::Docker),
    ("docs", FolderIconKind::Docs),
    ("fixtures", FolderIconKind::Tests),
    ("fonts", FolderIconKind::Assets),
    ("images", FolderIconKind::Assets),
    ("lib", FolderIconKind::Source),
    ("migrations", FolderIconKind::Database),
    ("node_modules", FolderIconKind::Dependencies),
    ("packages", FolderIconKind::Dependencies),
    ("public", FolderIconKind::Public),
    ("scripts", FolderIconKind::Scripts),
    ("server", FolderIconKind::Source),
    ("src", FolderIconKind::Source),
    ("static", FolderIconKind::Public),
    ("styles", FolderIconKind::Assets),
    ("test", FolderIconKind::Tests),
    ("tests", FolderIconKind::Tests),
    ("vendor", FolderIconKind::Dependencies),
    ("web", FolderIconKind::Public),
];

// Remote listings re-render on every navigation, selection, and transfer
// tick, and a name always resolves to the same icon.
const FILE_ICON_CACHE_LIMIT: usize = 4_000;

thread_local! {
    static RESOLVED_FILE_ICONS: RefCell<HashMap<String, FileIconKind>> = RefCell::new(HashMap::new());
}

/// Resolve a filename without touching file contents, suitable for large remote listings.
pub fn file_icon_kind(name: &str) -> FileIconKind {
    if let Some(kind) = RESOLVED_FILE_ICONS.with(|cache| cache.borrow().get(name).copied()) {
        return kind;
    }
    let kind = resolve_file_icon_kind(name);
    RESOLVED_FILE_ICONS.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() >= FILE_ICON_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(name.to_string(), kind);
    });
    kind
}

fn lookup<K: Copy>(table: &[(&str, K)], key: &str) -> Option<K> {
    table
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, kind)| *kind)
}

/// `/^(?:licen[cs]e|copying)(?:\.|$)/`
fn is_license_name(lower: &str) -> bool {
    ["license", "licence", "copying"].iter().any(|stem| {
        lower
            .strip_prefix(stem)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
    })
}

/// `/^(?:eslint|prettier|stylelint|tailwind|postcss)\.config\./`
fn is_tool_config(lower: &str) -> bool {
    ["eslint", "prettier", "stylelint", "tailwind", "postcss"]
        .iter()
        .any(|tool| {
            lower
                .strip_prefix(tool)
                .is_some_and(|rest| rest.starts_with(".config."))
        })
}

/// `/^(?:tsconfig|jsconfig)(?:\.[^.]+)*\.json$/`
fn is_ts_config(lower: &str) -> bool {
    let Some(rest) = lower
        .strip_prefix("tsconfig")
        .or_else(|| lower.strip_prefix("jsconfig"))
    else {
        return false;
    };
    let Some(middle) = rest.strip_suffix(".json") else {
        return false;
    };
    // `(?:\.[^.]+)*`: empty, or dot-separated non-empty segments.
    middle.is_empty()
        || (middle.starts_with('.') && middle[1..].split('.').all(|part| !part.is_empty()))
}

fn resolve_file_icon_kind(name: &str) -> FileIconKind {
    let lower = name.to_lowercase();

    // `/^\.env(?:\.|$)/`
    if lower == ".env" || lower.starts_with(".env.") {
        return FileIconKind::Env;
    }
    if lower.starts_with("docker-compose.") || lower.starts_with("compose.") {
        return FileIconKind::Docker;
    }
    if lower.starts_with("readme.") {
        return FileIconKind::Markdown;
    }
    if is_license_name(&lower) {
        return FileIconKind::License;
    }
    if is_tool_config(&lower) {
        return FileIconKind::Config;
    }
    if is_ts_config(&lower) {
        return FileIconKind::Typescript;
    }

    if let Some(exact) = lookup(EXACT_FILE_ICONS, &lower) {
        return exact;
    }

    match lower.rfind('.') {
        Some(index) if index != lower.len() - 1 => {
            lookup(EXTENSION_FILE_ICONS, &lower[index + 1..]).unwrap_or(FileIconKind::File)
        }
        _ => FileIconKind::File,
    }
}

/// Resolve familiar developer folder names to a stable visual category.
pub fn folder_icon_kind(name: &str) -> FolderIconKind {
    lookup(FOLDER_ICONS, &name.to_lowercase()).unwrap_or(FolderIconKind::Folder)
}

fn color(value: Option<&str>, fallback: Hsla) -> Hsla {
    value.map(hex).unwrap_or(fallback)
}

/// The short label over a badge: Inter at 7px (5.5px past two characters),
/// heavy and tightly tracked, centred in `inset` of the badge.
fn badge_label(text: &'static str, ink: Hsla, weight: f32, tracking: f32) -> impl IntoElement {
    let size = if text.chars().count() > 2 { 5.5 } else { 7. };
    label(text)
        .size(size)
        .weight(weight)
        .tracking(tracking)
        .line_height(size)
        .color(ink)
        .no_truncate()
}

fn file_badge(visual: FileVisual) -> AnyElement {
    if let Some(glyph) = visual.glyph {
        return icon(glyph, 19., hex(visual.color)).into_any_element();
    }
    let ink = color(visual.label_color, hex(super::palette::LABEL_INK));
    div()
        .relative()
        .flex_none()
        .size(px(20.))
        .child(
            div()
                .absolute()
                .inset_0()
                .child(icon("DescriptionRounded", 20., hex(visual.color))),
        )
        .children(visual.label.map(|text| {
            // `inset: 5px 2px 1px 1px`
            div()
                .absolute()
                .top(px(5.))
                .right(px(2.))
                .bottom(px(1.))
                .left(px(1.))
                .flex()
                .items_center()
                .justify_center()
                .child(badge_label(text, ink, 800., -0.35))
        }))
        .into_any_element()
}

fn folder_badge(name: &str) -> AnyElement {
    let visual = folder_icon_kind(name).visual();
    let ink = color(visual.label_color, hex(super::palette::LABEL_INK));
    div()
        .relative()
        .flex_none()
        .w(px(21.))
        .h(px(20.))
        .child(
            div()
                .absolute()
                .inset_0()
                .child(icon("FolderRounded", 21., hex(visual.color))),
        )
        .children(visual.label.map(|text| {
            // `inset: 7px 1px 1px`; the bundled Inter stops at 800.
            div()
                .absolute()
                .top(px(7.))
                .right(px(1.))
                .bottom(px(1.))
                .left(px(1.))
                .flex()
                .items_center()
                .justify_center()
                .child(badge_label(text, ink, 800., -0.45))
        }))
        .into_any_element()
}

/// `<FileTypeIcon name type />`: a compact icon for remote files, folders, and links.
pub fn file_type_icon(name: &str, kind: SftpEntryType) -> AnyElement {
    match kind {
        SftpEntryType::Dir => folder_badge(name),
        SftpEntryType::Link => {
            icon("LinkRounded", 19., hex(super::palette::LINK_COLOR)).into_any_element()
        }
        SftpEntryType::File | SftpEntryType::Other => file_badge(file_icon_kind(name).visual()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str) -> &'static str {
        file_icon_kind(name).name()
    }

    #[test]
    fn recognizes_developer_files_by_extension() {
        assert_eq!(file("App.tsx"), "react");
        assert_eq!(file("main.ts"), "typescript");
        assert_eq!(file("schema.graphql"), "graphql");
        assert_eq!(file("main.tf"), "terraform");
        assert_eq!(file("photo.webp"), "image");
        assert_eq!(file("backup.tar.gz"), "archive");
    }

    #[test]
    fn gives_special_project_files_distinct_icons() {
        assert_eq!(file("package.json"), "npm");
        assert_eq!(file("pnpm-lock.yaml"), "pnpm");
        assert_eq!(file("Dockerfile"), "docker");
        assert_eq!(file("docker-compose.yml"), "docker");
        assert_eq!(file("README.md"), "markdown");
        assert_eq!(file("LICENSE.txt"), "license");
        assert_eq!(file(".env.production"), "env");
        assert_eq!(file("tsconfig.node.json"), "typescript");
    }

    #[test]
    fn falls_back_safely_for_extensionless_and_unknown_files() {
        assert_eq!(file("deploy"), "file");
        assert_eq!(file("artifact.unknown"), "file");
        assert_eq!(file(".envoy"), "file");
    }

    #[test]
    fn recognizes_common_project_folders_case_insensitively() {
        assert_eq!(folder_icon_kind("src").name(), "source");
        assert_eq!(folder_icon_kind("__tests__").name(), "tests");
        assert_eq!(folder_icon_kind("node_modules").name(), "dependencies");
        assert_eq!(folder_icon_kind(".git").name(), "git");
        assert_eq!(folder_icon_kind(".SSH").name(), "secure");
        assert_eq!(folder_icon_kind("public").name(), "public");
    }

    #[test]
    fn uses_the_standard_folder_for_unfamiliar_names() {
        assert_eq!(folder_icon_kind("quarterly-reports").name(), "folder");
    }

    #[test]
    fn matches_the_pattern_rules_exactly() {
        assert_eq!(file("tsconfig..json"), "json");
        assert_eq!(file("jsconfig.json"), "typescript");
        assert_eq!(file("eslint.config.mjs"), "config");
        assert_eq!(file("licence"), "license");
        assert_eq!(file("licensed.txt"), "text");
        assert_eq!(file("archive."), "file");
        assert_eq!(file(".env"), "env");
    }
}
