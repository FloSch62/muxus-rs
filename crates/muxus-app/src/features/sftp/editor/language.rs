//! Which language a file is edited as: `client/src/editor/language-detection.ts`
//! (Monaco's language ids, kept so the status bar and the header show what the
//! client shows) and the tree-sitter grammar the native editor highlights
//! each of them with.

use std::sync::OnceLock;

use regex::Regex;

/// Monaco's id for the app's general text language (logs, notes, plain configs).
pub const GENERAL_TEXT_LANGUAGE_ID: &str = "muxus-text";

const FILE_NAME_LANGUAGES: &[(&str, &str)] = &[
    (".bash_aliases", "shell"),
    (".bash_login", "shell"),
    (".bash_logout", "shell"),
    (".bash_profile", "shell"),
    (".bashrc", "shell"),
    (".editorconfig", "ini"),
    (".env", "shell"),
    (".eslintrc", "json"),
    (".gitattributes", "ini"),
    (".gitconfig", "ini"),
    (".jscsrc", "json"),
    (".jshintrc", "json"),
    (".babelrc", "json"),
    (".npmrc", "ini"),
    (".profile", "shell"),
    (".rhistory", "r"),
    (".rprofile", "r"),
    (".zlogin", "shell"),
    (".zlogout", "shell"),
    (".zprofile", "shell"),
    (".zshenv", "shell"),
    (".zshrc", "shell"),
    ("containerfile", "dockerfile"),
    ("dockerfile", "dockerfile"),
    ("gemfile", "ruby"),
    ("jakefile", "javascript"),
    ("kubeconfig", "yaml"),
    ("rakefile", "ruby"),
    ("ssh_config", "ini"),
    ("sshd_config", "ini"),
];

const EXTENSION_LANGUAGES: &[(&str, &str)] = &[
    (".abap", "abap"),
    (".aes", "aes"),
    (".ascx", "xml"),
    (".asp", "html"),
    (".aspx", "html"),
    (".azcli", "azcli"),
    (".bash", "shell"),
    (".bat", "bat"),
    (".bicep", "bicep"),
    (".bowerrc", "json"),
    (".c", "c"),
    (".cake", "csharp"),
    (".cc", "cpp"),
    (".cjs", "javascript"),
    (".clj", "clojure"),
    (".cljc", "clojure"),
    (".cljs", "clojure"),
    (".cls", "apex"),
    (".cmd", "bat"),
    (".coffee", "coffeescript"),
    (".conf", "ini"),
    (".config", "xml"),
    (".cpp", "cpp"),
    (".cpy", "python"),
    (".cs", "csharp"),
    (".cshtml", "razor"),
    (".csproj", "xml"),
    (".csp", "csp"),
    (".css", "css"),
    (".csx", "csharp"),
    (".ctp", "php"),
    (".cts", "typescript"),
    (".cxx", "cpp"),
    (".cyp", "cypher"),
    (".cypher", "cypher"),
    (".dart", "dart"),
    (".dats", "postiats"),
    (".dax", "msdax"),
    (".dockerfile", "dockerfile"),
    (".desktop", "ini"),
    (".dtd", "xml"),
    (".ecl", "ecl"),
    (".edn", "clojure"),
    (".es6", "javascript"),
    (".ex", "elixir"),
    (".exs", "elixir"),
    (".flow", "flow9"),
    (".fs", "fsharp"),
    (".fsi", "fsharp"),
    (".fsscript", "fsharp"),
    (".fsx", "fsharp"),
    (".ftl", "freemarker2"),
    (".ftlh", "freemarker2"),
    (".ftlx", "freemarker2"),
    (".gemspec", "ruby"),
    (".gitconfig", "ini"),
    (".go", "go"),
    (".gql", "graphql"),
    (".graphql", "graphql"),
    (".gyp", "python"),
    (".gypi", "python"),
    (".h", "c"),
    (".handlebars", "handlebars"),
    (".har", "json"),
    (".hats", "postiats"),
    (".hbs", "handlebars"),
    (".hcl", "hcl"),
    (".hh", "cpp"),
    (".hpp", "cpp"),
    (".htm", "html"),
    (".html", "html"),
    (".html.liquid", "liquid"),
    (".hxx", "cpp"),
    (".i3", "m3"),
    (".iecplc", "st"),
    (".iecst", "st"),
    (".ig", "m3"),
    (".ini", "ini"),
    (".jade", "pug"),
    (".jav", "java"),
    (".java", "java"),
    (".js", "javascript"),
    (".jscsrc", "json"),
    (".jshtm", "html"),
    (".jshintrc", "json"),
    (".json", "json"),
    (".jsonc", "json"),
    (".jsp", "html"),
    (".jsx", "javascript"),
    (".jl", "julia"),
    (".kt", "kotlin"),
    (".kts", "kotlin"),
    (".lc3lib", "st"),
    (".less", "less"),
    (".lex", "lexon"),
    (".ligo", "pascaligo"),
    (".liquid", "liquid"),
    (".lua", "lua"),
    (".m", "objective-c"),
    (".m3", "m3"),
    (".markdown", "markdown"),
    (".md", "markdown"),
    (".mdoc", "html"),
    (".mdown", "markdown"),
    (".mdtext", "markdown"),
    (".mdtxt", "markdown"),
    (".mdwn", "markdown"),
    (".mdx", "mdx"),
    (".mg", "m3"),
    (".mjs", "javascript"),
    (".mkd", "markdown"),
    (".mkdn", "markdown"),
    (".ml", "fsharp"),
    (".mligo", "cameligo"),
    (".mli", "fsharp"),
    (".msdax", "msdax"),
    (".mts", "typescript"),
    (".opf", "xml"),
    (".p", "pascal"),
    (".pas", "pascal"),
    (".php", "php"),
    (".php4", "php"),
    (".php5", "php"),
    (".phtml", "php"),
    (".pl", "perl"),
    (".pla", "pla"),
    (".pm", "perl"),
    (".pp", "ruby"),
    (".pq", "powerquery"),
    (".pqm", "powerquery"),
    (".properties", "ini"),
    (".props", "xml"),
    (".proto", "proto"),
    (".ps1", "powershell"),
    (".psd1", "powershell"),
    (".psm1", "powershell"),
    (".pug", "pug"),
    (".py", "python"),
    (".pyw", "python"),
    (".qs", "qsharp"),
    (".r", "r"),
    (".rb", "ruby"),
    (".rbx", "ruby"),
    (".redis", "redis"),
    (".rhistory", "r"),
    (".rjs", "ruby"),
    (".rkt", "scheme"),
    (".rlib", "rust"),
    (".rmd", "r"),
    (".rprofile", "r"),
    (".rpy", "python"),
    (".rq", "sparql"),
    (".rs", "rust"),
    (".rst", "restructuredtext"),
    (".rt", "r"),
    (".s", "mips"),
    (".sats", "postiats"),
    (".sb", "sb"),
    (".sbt", "scala"),
    (".sc", "scala"),
    (".scala", "scala"),
    (".sch", "scheme"),
    (".scm", "scheme"),
    (".scss", "scss"),
    (".sh", "shell"),
    (".shtml", "html"),
    (".sol", "sol"),
    (".sql", "sql"),
    (".ss", "scheme"),
    (".st", "st"),
    (".sv", "systemverilog"),
    (".svg", "xml"),
    (".svgz", "xml"),
    (".svh", "systemverilog"),
    (".swift", "swift"),
    (".targets", "xml"),
    (".tcl", "tcl"),
    (".tcdut", "st"),
    (".tcgvl", "st"),
    (".tcio", "st"),
    (".tcpou", "st"),
    (".tf", "hcl"),
    (".tfvars", "hcl"),
    (".timer", "ini"),
    (".toml", "ini"),
    (".ts", "typescript"),
    (".tsp", "typespec"),
    (".tsx", "typescript"),
    (".twig", "twig"),
    (".v", "verilog"),
    (".vb", "vb"),
    (".vh", "verilog"),
    (".wgsl", "wgsl"),
    (".wxi", "xml"),
    (".wxl", "xml"),
    (".wxs", "xml"),
    (".xaml", "xml"),
    (".xhtml", "html"),
    (".xml", "xml"),
    (".xsd", "xml"),
    (".xsl", "xml"),
    (".xslt", "xml"),
    (".yaml", "yaml"),
    (".yml", "yaml"),
];

/// Monaco's built-in languages and the first alias it labels each with.
const MONACO_LANGUAGES: &[(&str, &str)] = &[
    ("abap", "abap"),
    ("aes", "aes"),
    ("apex", "Apex"),
    ("azcli", "Azure CLI"),
    ("bat", "Batch"),
    ("bicep", "Bicep"),
    ("c", "C"),
    ("cameligo", "Cameligo"),
    ("clojure", "clojure"),
    ("coffeescript", "CoffeeScript"),
    ("cpp", "C++"),
    ("csharp", "C#"),
    ("csp", "CSP"),
    ("css", "CSS"),
    ("cypher", "Cypher"),
    ("dart", "Dart"),
    ("dockerfile", "Dockerfile"),
    ("ecl", "ECL"),
    ("elixir", "Elixir"),
    ("flow9", "Flow9"),
    ("freemarker2", "FreeMarker2"),
    ("fsharp", "F#"),
    ("go", "Go"),
    ("graphql", "GraphQL"),
    ("handlebars", "Handlebars"),
    ("hcl", "Terraform"),
    ("html", "HTML"),
    ("ini", "Ini"),
    ("java", "Java"),
    ("javascript", "JavaScript"),
    ("json", "JSON"),
    ("julia", "julia"),
    ("kotlin", "Kotlin"),
    ("less", "Less"),
    ("lexon", "Lexon"),
    ("liquid", "Liquid"),
    ("lua", "Lua"),
    ("m3", "Modula-3"),
    ("markdown", "Markdown"),
    ("mdx", "MDX"),
    ("mips", "MIPS"),
    ("msdax", "DAX"),
    ("muxus-text", "General Text"),
    ("mysql", "MySQL"),
    ("objective-c", "Objective-C"),
    ("pascal", "Pascal"),
    ("pascaligo", "Pascaligo"),
    ("perl", "Perl"),
    ("pgsql", "PostgreSQL"),
    ("php", "PHP"),
    ("postiats", "ATS"),
    ("powerquery", "PQ"),
    ("powershell", "PowerShell"),
    ("proto", "protobuf"),
    ("pug", "Pug"),
    ("python", "Python"),
    ("qsharp", "Q#"),
    ("r", "R"),
    ("razor", "Razor"),
    ("redis", "redis"),
    ("redshift", "Redshift"),
    ("restructuredtext", "reStructuredText"),
    ("ruby", "Ruby"),
    ("rust", "Rust"),
    ("sb", "Small Basic"),
    ("scala", "Scala"),
    ("scheme", "scheme"),
    ("scss", "Sass"),
    ("shell", "Shell"),
    ("sol", "sol"),
    ("sparql", "sparql"),
    ("sql", "SQL"),
    ("st", "StructuredText"),
    ("swift", "Swift"),
    ("systemverilog", "SV"),
    ("tcl", "tcl"),
    ("twig", "Twig"),
    ("typescript", "TypeScript"),
    ("typespec", "TypeSpec"),
    ("vb", "Visual Basic"),
    ("verilog", "V"),
    ("wgsl", "WebGPU Shading Language"),
    ("xml", "XML"),
    ("yaml", "YAML"),
];

fn first_line_rules() -> &'static [(Regex, &'static str)] {
    static RULES: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    RULES.get_or_init(|| {
        [
            (r"(?i)^#!.*\b(?:node|deno|bun)\b", "javascript"),
            (r"(?i)^#!.*\bpython[0-9.-]*\b", "python"),
            (r"(?i)^#!.*\b(?:ruby|jruby)\b", "ruby"),
            (r"(?i)^#!.*\bperl\b", "perl"),
            (r"(?i)^#!.*\b(?:pwsh|powershell)\b", "powershell"),
            (r"(?i)^#!.*\b(?:ba|da|k|z)?sh\b", "shell"),
            (r"(?i)^\s*(?:<\?xml\b|<svg\b|<!doctype\s+svg\b)", "xml"),
        ]
        .into_iter()
        .map(|(pattern, language)| (Regex::new(pattern).expect("a valid rule"), language))
        .collect()
    })
}

fn language_from_first_line(content: &str) -> Option<&'static str> {
    let first_line = content.split('\n').next().unwrap_or("");
    first_line_rules()
        .iter()
        .find(|(rule, _)| rule.is_match(first_line))
        .map(|(_, language)| *language)
}

fn path_rules() -> &'static [(Regex, &'static str)] {
    static RULES: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    RULES.get_or_init(|| {
        [
            (r"(?:^|/)\.kube/config$", "yaml"),
            (r"(?:^|/)kubeconfig(?:-[^/]*)?$", "yaml"),
            (r"^/etc/kubernetes/[^/]+\.conf$", "yaml"),
            (r"(?:^|/)\.aws/(?:config|credentials)$", "ini"),
        ]
        .into_iter()
        .map(|(pattern, language)| (Regex::new(pattern).expect("a valid rule"), language))
        .collect()
    })
}

/// Extensions longest first, so compound extensions win (`.html.liquid` over `.html`).
fn extensions_by_length() -> &'static [(&'static str, &'static str)] {
    static SORTED: OnceLock<Vec<(&'static str, &'static str)>> = OnceLock::new();
    SORTED.get_or_init(|| {
        let mut sorted = EXTENSION_LANGUAGES.to_vec();
        // A stable sort keeps the table's order among equal lengths, as `Array.sort` does.
        sorted.sort_by_key(|(extension, _)| std::cmp::Reverse(extension.chars().count()));
        sorted
    })
}

/// Resolve a file to one of Monaco's built-in language ids.
pub fn language_for_path(path: &str, content: &str) -> &'static str {
    let lower_path = path.to_lowercase().replace('\\', "/");
    if let Some((_, language)) = path_rules()
        .iter()
        .find(|(rule, _)| rule.is_match(&lower_path))
    {
        return language;
    }

    let name = &path[path.rfind('/').map_or(0, |index| index + 1)..];
    let lower_name = name.to_lowercase();
    if lower_name.starts_with(".env.") {
        return "shell";
    }
    if let Some((_, language)) = FILE_NAME_LANGUAGES
        .iter()
        .find(|(file, _)| *file == lower_name)
    {
        return language;
    }
    for (extension, language) in extensions_by_length() {
        if lower_name.ends_with(extension) {
            return language;
        }
    }
    language_from_first_line(content).unwrap_or(GENERAL_TEXT_LANGUAGE_ID)
}

/// How the header names a language: Monaco's id, or "General text".
pub fn header_language_label(language: &str) -> String {
    if language == GENERAL_TEXT_LANGUAGE_ID {
        "General text".into()
    } else {
        language.into()
    }
}

/// The status bar's language picker: Plain Text first, then every built-in
/// language by its first alias (`languageOptions`).
pub fn language_options() -> &'static [(&'static str, &'static str)] {
    static OPTIONS: OnceLock<Vec<(&'static str, &'static str)>> = OnceLock::new();
    OPTIONS.get_or_init(|| {
        let mut options: Vec<(&'static str, &'static str)> = MONACO_LANGUAGES.to_vec();
        options.sort_by(|left, right| muxus_core::collate::locale_compare(left.1, right.1));
        options.insert(0, ("plaintext", "Plain Text"));
        options
    })
}

/// The status bar's label for a language id.
pub fn language_label(language: &str) -> &'static str {
    language_options()
        .iter()
        .find(|(id, _)| *id == language)
        .map(|(_, label)| *label)
        .unwrap_or("Plain Text")
}

/// The tree-sitter grammar the native editor highlights a Monaco language
/// with; `text` where no grammar is bundled.
pub fn highlighter_language(language: &str, path: &str) -> &'static str {
    match language {
        "shell" => "bash",
        "javascript" => "javascript",
        "typescript" if path.to_lowercase().ends_with(".tsx") => "tsx",
        "typescript" => "typescript",
        "json" => "json",
        "yaml" => "yaml",
        "python" => "python",
        "rust" => "rust",
        "go" => "go",
        "c" => "c",
        "cpp" => "cpp",
        "csharp" => "csharp",
        "css" | "scss" | "less" => "css",
        "html" => "html",
        "java" => "java",
        "kotlin" => "kotlin",
        "lua" => "lua",
        "markdown" => "markdown",
        "php" => "php",
        "proto" => "proto",
        "ruby" => "ruby",
        "scala" => "scala",
        "sql" | "mysql" | "pgsql" | "redshift" => "sql",
        "swift" => "swift",
        "graphql" => "graphql",
        "elixir" => "elixir",
        _ => "text",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_common_and_compound_source_extensions() {
        assert_eq!(language_for_path("/srv/app/main.tsx", ""), "typescript");
        assert_eq!(language_for_path("/srv/app/site.html.liquid", ""), "liquid");
        assert_eq!(language_for_path("/infra/main.tf", ""), "hcl");
        assert_eq!(language_for_path("/src/App.csproj", ""), "xml");
        assert_eq!(language_for_path("/api/schema.graphql", ""), "graphql");
        assert_eq!(language_for_path("/srv/app/pyproject.toml", ""), "ini");
    }

    #[test]
    fn recognizes_conventional_extensionless_configuration_and_script_names() {
        assert_eq!(language_for_path("/srv/app/Dockerfile", ""), "dockerfile");
        assert_eq!(
            language_for_path("/srv/app/Containerfile", ""),
            "dockerfile"
        );
        assert_eq!(language_for_path("/home/alice/.zshrc", ""), "shell");
        assert_eq!(language_for_path("/srv/app/.env.production", ""), "shell");
        assert_eq!(language_for_path("/etc/ssh/sshd_config", ""), "ini");
        assert_eq!(language_for_path("/home/alice/.kube/config", ""), "yaml");
        assert_eq!(language_for_path("/etc/kubernetes/admin.conf", ""), "yaml");
        assert_eq!(language_for_path("/home/alice/.aws/credentials", ""), "ini");
    }

    #[test]
    fn uses_a_shebang_or_xml_prologue_when_the_filename_is_ambiguous() {
        assert_eq!(
            language_for_path(
                "/usr/local/bin/deploy",
                "#!/usr/bin/env python3\nprint(\"ok\")"
            ),
            "python"
        );
        assert_eq!(
            language_for_path("/usr/local/bin/start", "#!/bin/zsh\nexec app"),
            "shell"
        );
        assert_eq!(
            language_for_path("/tmp/vector", "<?xml version=\"1.0\"?>\n<svg />"),
            "xml"
        );
    }

    #[test]
    fn falls_back_to_general_highlighting_for_text_and_unsupported_formats() {
        assert_eq!(
            language_for_path("/notes/readme.txt", ""),
            GENERAL_TEXT_LANGUAGE_ID
        );
        assert_eq!(
            language_for_path("/var/log/application.log", ""),
            GENERAL_TEXT_LANGUAGE_ID
        );
        assert_eq!(
            language_for_path("/etc/hosts", "127.0.0.1 localhost"),
            GENERAL_TEXT_LANGUAGE_ID
        );
    }

    #[test]
    fn labels_languages_as_monaco_does() {
        assert_eq!(language_label("yaml"), "YAML");
        assert_eq!(language_label("shell"), "Shell");
        assert_eq!(language_label(GENERAL_TEXT_LANGUAGE_ID), "General Text");
        assert_eq!(language_options()[0], ("plaintext", "Plain Text"));
        assert_eq!(header_language_label("yaml"), "yaml");
        assert_eq!(
            header_language_label(GENERAL_TEXT_LANGUAGE_ID),
            "General text"
        );
    }

    #[test]
    fn highlights_with_the_matching_grammar() {
        assert_eq!(highlighter_language("yaml", "/a.yml"), "yaml");
        assert_eq!(highlighter_language("shell", "/deploy.sh"), "bash");
        assert_eq!(highlighter_language("typescript", "/App.tsx"), "tsx");
        assert_eq!(highlighter_language("ini", "/a.ini"), "text");
    }
}
