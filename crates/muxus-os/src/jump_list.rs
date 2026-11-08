//! The Windows taskbar jump list, set and read the way Electron's `app.setJumpList` and
//! `app.getJumpListSettings` do (`shell/browser/ui/win/jump_list.cc`): a custom destination
//! list for the process's AppUserModelID, with custom categories and the Tasks category made
//! of shell links back to the executable.
//!
//! GPUI's own `update_jump_list` only offers Zed's "Recent Folders" and `--dock-action` tasks,
//! so the list is built here. What goes into it is `muxus-app`'s `launch_menu`.

/// One entry: a shell link that starts `program` with `args` (`app.setJumpList`'s task).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpListTask {
    pub title: String,
    /// The tooltip.
    pub description: String,
    pub program: String,
    /// The command line after the program, already quoted for `CommandLineToArgvW`.
    pub args: String,
    pub icon_path: String,
    pub icon_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JumpListCategory {
    /// A named category of its own.
    Custom {
        name: String,
        items: Vec<JumpListTask>,
    },
    /// The standard Tasks category at the bottom.
    Tasks { items: Vec<JumpListTask> },
}

impl JumpListCategory {
    pub fn items(&self) -> &[JumpListTask] {
        match self {
            Self::Custom { items, .. } | Self::Tasks { items } => items,
        }
    }
}

/// What setting the list reported (`app.setJumpList`'s result).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JumpListResult {
    Ok,
    /// Windows refused a custom category because the user turned off "Show recently opened
    /// items in Start, Jump Lists and File Explorer".
    CustomCategoryAccessDenied,
    /// A category held a file whose type the app is not registered for.
    FileTypeRegistration,
    Error,
}

impl JumpListResult {
    /// Electron's name for the result, which the main log quotes.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::CustomCategoryAccessDenied => "customCategoryAccessDeniedError",
            Self::FileTypeRegistration => "fileTypeRegistrationError",
            Self::Error => "error",
        }
    }
}

/// Whether this platform has a taskbar jump list.
pub const SUPPORTED: bool = cfg!(windows);

/// `E_ACCESSDENIED`: custom categories are off in the privacy settings.
const E_ACCESSDENIED: i32 = 0x8007_0005_u32 as i32;
/// `DESTS_E_NO_MATCHING_ASSOC_HANDLER`: a file whose type the app is not registered for.
const DESTS_E_NO_MATCHING_ASSOC_HANDLER: i32 = 0x8004_0F03_u32 as i32;

/// The result after adding one category's items failed with `hresult`, given the result so
/// far: the two specific failures of a custom category win, anything else keeps an earlier
/// failure (Electron's `AppendCategory`).
pub fn append_failure(custom: bool, hresult: i32, so_far: JumpListResult) -> JumpListResult {
    match hresult {
        DESTS_E_NO_MATCHING_ASSOC_HANDLER if custom => JumpListResult::FileTypeRegistration,
        E_ACCESSDENIED if custom => JumpListResult::CustomCategoryAccessDenied,
        _ if so_far == JumpListResult::Ok => JumpListResult::Error,
        _ => so_far,
    }
}

/// The list's result from its categories' results: the first failure that says more than
/// "error", else "error" (Electron's `AppendCategories`). A failed commit turns a success into
/// an error and keeps an earlier, more telling failure.
pub fn list_result(
    categories: impl IntoIterator<Item = JumpListResult>,
    committed: bool,
) -> JumpListResult {
    let result = categories
        .into_iter()
        .fold(JumpListResult::Ok, |so_far, category| {
            let replaceable = matches!(so_far, JumpListResult::Ok | JumpListResult::Error);
            if replaceable && category != JumpListResult::Ok {
                category
            } else {
                so_far
            }
        });
    if !committed && result == JumpListResult::Ok {
        JumpListResult::Error
    } else {
        result
    }
}

/// The arguments of the entries the user removed from the list since it was last set
/// (`app.getJumpListSettings().removedItems`). Windows reports a removal only until the list is
/// next set, so callers keep them. Errors off Windows.
pub fn removed_jump_list_args(app_id: &str) -> Result<Vec<String>, String> {
    #[cfg(windows)]
    {
        windows::removed_args(app_id)
    }
    #[cfg(not(windows))]
    {
        let _ = app_id;
        Err("only Windows has a taskbar jump list".into())
    }
}

/// Replace the jump list of the AppUserModelID `app_id` with `categories`, in order
/// (`app.setJumpList`). Empty categories are skipped by the caller. Errors off Windows.
pub fn set_jump_list(app_id: &str, categories: &[JumpListCategory]) -> JumpListResult {
    #[cfg(windows)]
    {
        windows::set(app_id, categories)
    }
    #[cfg(not(windows))]
    {
        let _ = (app_id, categories);
        JumpListResult::Error
    }
}

#[cfg(windows)]
mod windows;

#[cfg(test)]
mod tests {
    use super::*;

    const E_FAIL: i32 = 0x8000_4005_u32 as i32;

    #[test]
    fn names_results_as_electron_does() {
        assert_eq!(JumpListResult::Ok.as_str(), "ok");
        assert_eq!(
            JumpListResult::CustomCategoryAccessDenied.as_str(),
            "customCategoryAccessDeniedError"
        );
        assert_eq!(
            JumpListResult::FileTypeRegistration.as_str(),
            "fileTypeRegistrationError"
        );
        assert_eq!(JumpListResult::Error.as_str(), "error");
    }

    #[test]
    fn a_custom_category_refused_for_privacy_says_so() {
        assert_eq!(
            append_failure(true, E_ACCESSDENIED, JumpListResult::Ok),
            JumpListResult::CustomCategoryAccessDenied
        );
        assert_eq!(
            append_failure(true, DESTS_E_NO_MATCHING_ASSOC_HANDLER, JumpListResult::Ok),
            JumpListResult::FileTypeRegistration
        );
    }

    #[test]
    fn other_failures_are_generic_and_keep_an_earlier_one() {
        assert_eq!(
            append_failure(true, E_FAIL, JumpListResult::Ok),
            JumpListResult::Error
        );
        // The Tasks category has no privacy switch.
        assert_eq!(
            append_failure(false, E_ACCESSDENIED, JumpListResult::Ok),
            JumpListResult::Error
        );
        assert_eq!(
            append_failure(false, E_FAIL, JumpListResult::CustomCategoryAccessDenied),
            JumpListResult::CustomCategoryAccessDenied
        );
    }

    #[test]
    fn the_list_reports_its_first_failure_and_a_failed_commit() {
        use JumpListResult::*;
        assert_eq!(list_result([Ok, Ok], true), Ok);
        assert_eq!(
            list_result([Ok, CustomCategoryAccessDenied, Error], true),
            CustomCategoryAccessDenied
        );
        assert_eq!(
            list_result([Error, CustomCategoryAccessDenied], true),
            CustomCategoryAccessDenied
        );
        assert_eq!(
            list_result([FileTypeRegistration, CustomCategoryAccessDenied], true),
            FileTypeRegistration
        );
        assert_eq!(list_result([Ok], false), Error);
        assert_eq!(
            list_result([CustomCategoryAccessDenied], false),
            CustomCategoryAccessDenied
        );
        assert_eq!(list_result([], true), Ok);
    }

    #[cfg(not(windows))]
    #[test]
    fn other_platforms_have_no_jump_list() {
        const { assert!(!SUPPORTED) };
        assert!(removed_jump_list_args("io.github.flosch62.muxus").is_err());
        assert_eq!(
            set_jump_list("io.github.flosch62.muxus", &[]),
            JumpListResult::Error
        );
    }
}
