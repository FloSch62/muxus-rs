//! A gNMI path set for reading (`PathText.tsx`): separators and key names
//! recede, key values stand out, YANG module prefixes are dropped unless
//! asked for.

use muxus_core::gnmi_path::{GnmiPath, strip_module_prefix, try_parse_gnmi_path};

/// How a piece of a path is colored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathTone {
    /// `text.secondary`: the origin, a lone `/`.
    Secondary,
    /// `text.disabled`: separators, module prefixes, key names.
    Disabled,
    /// The element names, in the surrounding text color.
    Name,
    /// Key values: `secondary.main` at weight 550.
    KeyValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathSegment {
    pub text: String,
    pub tone: PathTone,
}

fn segment(text: impl Into<String>, tone: PathTone) -> PathSegment {
    PathSegment {
        text: text.into(),
        tone,
    }
}

/// The pieces of a parsed path, `skip` leading elements left out.
pub fn path_segments(path: &GnmiPath, skip: usize, show_modules: bool) -> Vec<PathSegment> {
    let mut out = Vec::new();
    if skip == 0
        && let Some(origin) = path.origin.as_deref().filter(|origin| !origin.is_empty())
    {
        out.push(segment(format!("{origin}:"), PathTone::Secondary));
    }
    let elems = path.elems.get(skip..).unwrap_or_default();
    if elems.is_empty() {
        out.push(segment("/", PathTone::Secondary));
    }
    for elem in elems {
        out.push(segment("/", PathTone::Disabled));
        if show_modules && let Some(colon) = elem.name.find(':') {
            out.push(segment(&elem.name[..=colon], PathTone::Disabled));
        }
        out.push(segment(strip_module_prefix(&elem.name), PathTone::Name));
        for (key, value) in elem.keys.iter().flat_map(|keys| keys.iter()) {
            out.push(segment(format!("[{key}="), PathTone::Disabled));
            out.push(segment(value, PathTone::KeyValue));
            out.push(segment("]", PathTone::Disabled));
        }
    }
    out
}

/// Path text that may not parse: the parsed pieces, or the text as it is.
pub fn text_segments(text: &str, skip: usize, show_modules: bool) -> Option<Vec<PathSegment>> {
    try_parse_gnmi_path(text).map(|path| path_segments(&path, skip, show_modules))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(segments: &[PathSegment]) -> String {
        segments
            .iter()
            .map(|segment| match segment.tone {
                PathTone::KeyValue => format!("<{}>", segment.text),
                _ => segment.text.clone(),
            })
            .collect()
    }

    #[test]
    fn drops_module_prefixes_and_marks_key_values() {
        let segments = text_segments(
            "/srl_nokia-interfaces:interface[name=ethernet-1/1]/statistics",
            0,
            false,
        )
        .unwrap();
        assert_eq!(
            render(&segments),
            "/interface[name=<ethernet-1/1>]/statistics"
        );
        let segments = text_segments("/a:b/c", 0, true).unwrap();
        assert_eq!(render(&segments), "/a:b/c");
    }

    #[test]
    fn skips_leading_elements_and_shows_the_origin() {
        let segments = text_segments("openconfig:/interfaces/interface", 1, false).unwrap();
        assert_eq!(render(&segments), "/interface");
        let segments = text_segments("openconfig:/interfaces", 0, false).unwrap();
        assert_eq!(render(&segments), "openconfig:/interfaces");
        let segments = text_segments("/", 0, false).unwrap();
        assert_eq!(render(&segments), "/");
        assert_eq!(segments[0].tone, PathTone::Secondary);
        assert!(text_segments("/a[b", 0, false).is_none());
    }
}
