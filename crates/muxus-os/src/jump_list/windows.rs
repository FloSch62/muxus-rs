//! `ICustomDestinationList` calls (port of Electron's `shell/browser/ui/win/jump_list.cc`).
//!
//! Every call runs on a short-lived thread of its own with COM initialized as a single-threaded
//! apartment, so it never depends on how the caller's thread (GPUI's executor) set up COM.

use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize,
};
use windows::Win32::UI::Shell::Common::{IObjectArray, IObjectCollection};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{
    DestinationList, EnumerableObjectCollection, ICustomDestinationList, IShellLinkW, ShellLink,
};
use windows::core::{GUID, HSTRING, Interface};

use super::{JumpListCategory, JumpListResult, JumpListTask, append_failure, list_result};

/// `PKEY_Title` (propkey.h), the entry's label.
const PKEY_TITLE: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xf29f85e0_4ff9_1068_ab91_08002b27b3d9),
    pid: 2,
};

/// Longest command line Windows accepts, in UTF-16 units: room for any removed entry's
/// arguments.
const MAX_ARGUMENTS: usize = 32_768;

/// COM for the current thread, released when dropped if this initialized it.
struct ComApartment {
    initialized: bool,
}

impl ComApartment {
    fn enter() -> Self {
        // SAFETY: no reserved pointer is passed. S_OK and S_FALSE both need a matching
        // CoUninitialize, which `drop` makes; on RPC_E_CHANGED_MODE the thread already has a
        // multithreaded apartment that the calls below can use as it is.
        let result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        Self {
            initialized: result.is_ok(),
        }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: balances the successful CoInitializeEx of `enter` on this thread; every
            // COM object of the work closure has been released by now.
            unsafe { CoUninitialize() };
        }
    }
}

/// Run `work` on a thread of its own inside a COM apartment. `None` if it panicked.
fn in_apartment<T: Send>(work: impl FnOnce() -> T + Send) -> Option<T> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let _apartment = ComApartment::enter();
                work()
            })
            .join()
            .ok()
    })
}

fn destination_list(app_id: &str) -> windows::core::Result<ICustomDestinationList> {
    // SAFETY: plain COM activation of the shell's destination list class.
    let list: ICustomDestinationList =
        unsafe { CoCreateInstance(&DestinationList, None, CLSCTX_INPROC_SERVER) }?;
    // SAFETY: the HSTRING outlives the call.
    unsafe { list.SetAppID(&HSTRING::from(app_id)) }?;
    Ok(list)
}

pub(super) fn removed_args(app_id: &str) -> Result<Vec<String>, String> {
    in_apartment(|| -> windows::core::Result<Vec<String>> {
        let list = destination_list(app_id)?;
        let mut slots = 0u32;
        // SAFETY: `slots` is a valid out pointer for the call.
        let removed: IObjectArray = unsafe { list.BeginList(&mut slots) }?;
        let args = shell_link_arguments(&removed);
        // Nothing was added: drop the transaction, as `getJumpListSettings` does.
        // SAFETY: a list begun above.
        let _ = unsafe { list.AbortList() };
        Ok(args)
    })
    .ok_or_else(|| "the jump list thread panicked".to_string())?
    .map_err(|error| error.to_string())
}

/// The arguments of the shell links in `items`. Removed files are shell items without
/// arguments and are left out.
fn shell_link_arguments(items: &IObjectArray) -> Vec<String> {
    // SAFETY: a valid object array handed back by BeginList.
    let count = unsafe { items.GetCount() }.unwrap_or(0);
    let mut buffer = vec![0u16; MAX_ARGUMENTS];
    (0..count)
        .filter_map(|index| {
            // SAFETY: `index` is below the array's count.
            let link: IShellLinkW = unsafe { items.GetAt(index) }.ok()?;
            buffer.fill(0);
            // SAFETY: the slice is the buffer the call fills, NUL-terminated within it.
            unsafe { link.GetArguments(&mut buffer) }.ok()?;
            let length = buffer.iter().position(|&unit| unit == 0).unwrap_or(0);
            Some(String::from_utf16_lossy(&buffer[..length]))
        })
        .collect()
}

pub(super) fn set(app_id: &str, categories: &[JumpListCategory]) -> JumpListResult {
    in_apartment(|| {
        let Ok(list) = destination_list(app_id) else {
            return JumpListResult::Error;
        };
        let mut slots = 0u32;
        // SAFETY: `slots` is a valid out pointer; the removed items are not needed here.
        if unsafe { list.BeginList::<IObjectArray>(&mut slots) }.is_err() {
            return JumpListResult::Error;
        }
        let results: Vec<JumpListResult> = categories
            .iter()
            .map(|category| append_category(&list, category))
            .collect();
        // A list with a failed category is still better than none, so it is committed anyway.
        // SAFETY: a list begun above.
        let committed = unsafe { list.CommitList() }.is_ok();
        list_result(results, committed)
    })
    .unwrap_or(JumpListResult::Error)
}

fn append_category(list: &ICustomDestinationList, category: &JumpListCategory) -> JumpListResult {
    // SAFETY: plain COM activation of the shell's object collection class.
    let collection: windows::core::Result<IObjectCollection> =
        unsafe { CoCreateInstance(&EnumerableObjectCollection, None, CLSCTX_INPROC_SERVER) };
    let Ok(collection) = collection else {
        return JumpListResult::Error;
    };
    let mut result = JumpListResult::Ok;
    for task in category.items() {
        // SAFETY: a valid collection and a shell link created for it.
        let added = shell_link(task).and_then(|link| unsafe { collection.AddObject(&link) });
        if added.is_err() {
            result = JumpListResult::Error;
        }
    }
    let appended = match category {
        // SAFETY: the collection is an object array of shell links.
        JumpListCategory::Tasks { .. } => unsafe { list.AddUserTasks(&collection) },
        // SAFETY: as above; the HSTRING outlives the call.
        JumpListCategory::Custom { name, .. } => unsafe {
            list.AppendCategory(&HSTRING::from(name.as_str()), &collection)
        },
    };
    match appended {
        Ok(()) => result,
        Err(error) => append_failure(
            matches!(category, JumpListCategory::Custom { .. }),
            error.code().0,
            result,
        ),
    }
}

/// A shell link starting the task's program with its arguments, its tooltip, icon and title
/// (Electron's `ConvertToShellLink`).
fn shell_link(task: &JumpListTask) -> windows::core::Result<IShellLinkW> {
    // SAFETY: plain COM activation of the shell link class.
    let link: IShellLinkW = unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }?;
    // SAFETY: every HSTRING and the PROPVARIANT outlive the calls that read them.
    unsafe {
        link.SetPath(&HSTRING::from(task.program.as_str()))?;
        link.SetArguments(&HSTRING::from(task.args.as_str()))?;
        link.SetDescription(&HSTRING::from(task.description.as_str()))?;
        if !task.icon_path.is_empty() {
            let index = i32::try_from(task.icon_index).unwrap_or(0);
            link.SetIconLocation(&HSTRING::from(task.icon_path.as_str()), index)?;
        }
        let store: IPropertyStore = link.cast()?;
        let title = PROPVARIANT::from(task.title.as_str());
        store.SetValue(&PKEY_TITLE, &title)?;
        store.Commit()?;
    }
    Ok(link)
}
