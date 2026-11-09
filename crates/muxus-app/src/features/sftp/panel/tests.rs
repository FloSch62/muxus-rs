//! One remote operation at a time. A rename, new folder or delete started while a transfer
//! ran used to clear `busy` when it finished, under the transfer still going, so the next
//! transfer replaced `work` and so cancelled the first one without a word.

use std::cell::{Cell, RefCell};
use std::future::Future;

use gpui::{TestAppContext, WindowHandle};
use muxus_session::{Backend, BackendOptions};

use super::*;

const WAIT: &str = "Wait for the current SFTP operation to finish.";

struct Harness {
    window: WindowHandle<SftpPanel>,
    notes: Rc<RefCell<Vec<String>>>,
    _data: tempfile::TempDir,
    _home: tempfile::TempDir,
}

impl Harness {
    /// A panel on a sandboxed backend. Its connection does not exist: nothing here reaches a
    /// server, the operations are stand-ins the test finishes when it wants.
    fn start(cx: &mut TestAppContext) -> Self {
        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let backend =
            Backend::start_blocking(BackendOptions::sandbox(data.path(), home.path())).unwrap();
        // The backend answers the panel's first listing from its own threads.
        cx.executor().allow_parking();
        let notes: Rc<RefCell<Vec<String>>> = Rc::default();
        let sink = notes.clone();
        cx.update(|cx| {
            muxus_ui::init(muxus_ui::theme::Mode::Light, cx);
            super::super::services::init(
                backend,
                Rc::new(move |_, message, _, _| sink.borrow_mut().push(message.to_string())),
                cx,
            );
        });
        let window = cx.add_window(|window, cx| {
            SftpPanel::new(SftpPanelOptions::new("no-connection"), window, cx)
        });
        cx.run_until_parked();
        Self {
            window,
            notes,
            _data: data,
            _home: home,
        }
    }

    fn update<R>(
        &self,
        cx: &mut TestAppContext,
        f: impl FnOnce(&mut SftpPanel, &mut Window, &mut Context<SftpPanel>) -> R,
    ) -> R {
        self.window.update(cx, f).unwrap()
    }

    fn warnings(&self) -> usize {
        self.notes
            .borrow()
            .iter()
            .filter(|note| *note == WAIT)
            .count()
    }
}

/// An operation that finishes when the returned sender sends, and then sets `done`.
fn held_operation(
    done: Rc<Cell<bool>>,
) -> (
    async_channel::Sender<()>,
    impl Future<Output = Result<(), ApiError>> + 'static,
) {
    let (release, gate) = async_channel::bounded::<()>(1);
    let operation = async move {
        let _ = gate.recv().await;
        done.set(true);
        Ok(())
    };
    (release, operation)
}

fn file(name: &str) -> SftpEntry {
    SftpEntry {
        name: name.into(),
        kind: SftpEntryType::File,
        size: Some(1),
        mtime_ms: None,
        mode: None,
        owner: None,
        group: None,
        download_ticket: None,
    }
}

#[gpui::test]
fn a_second_operation_waits_for_the_first(cx: &mut TestAppContext) {
    let panel = Harness::start(cx);
    let (first_done, second_ran) = (Rc::new(Cell::new(false)), Rc::new(Cell::new(false)));
    let (release, first) = held_operation(first_done.clone());
    let second = {
        let second_ran = second_ran.clone();
        async move {
            second_ran.set(true);
            Ok(())
        }
    };
    panel.update(cx, |panel, _, cx| {
        panel.run(first, cx);
        panel.run(second, cx);
    });
    cx.run_until_parked();
    assert!(
        !second_ran.get(),
        "the second operation ran beside the first"
    );
    assert_eq!(panel.warnings(), 1);
    assert!(panel.update(cx, |panel, _, _| panel.busy));

    release.try_send(()).unwrap();
    cx.run_until_parked();
    assert!(first_done.get());
    assert!(!second_ran.get());
    assert!(!panel.update(cx, |panel, _, _| panel.busy));
}

#[gpui::test]
fn a_transfer_in_flight_outlives_a_rename_and_the_next_transfer(cx: &mut TestAppContext) {
    let panel = Harness::start(cx);
    // A transfer in flight, as the transfers hold one: `busy`, and its task in `work`,
    // which clears `busy` when it ends. Dropping the task cancels it.
    let finished = Rc::new(Cell::new(false));
    let (release, gate) = async_channel::bounded::<()>(1);
    panel.update(cx, |panel, _, cx| {
        panel.busy = true;
        let finished = finished.clone();
        panel.work = Some(cx.spawn(async move |this, cx| {
            let _ = gate.recv().await;
            finished.set(true);
            let _ = this.update(cx, |this, _| this.busy = false);
        }));
    });

    // New folder, rename and delete do not even ask.
    panel.update(cx, |panel, window, cx| {
        panel.mkdir(window, cx);
        panel.rename(file("a.txt"), window, cx);
        panel.remove(vec![file("a.txt")], window, cx);
        assert!(!panel.dialogs.is_open());
    });
    assert_eq!(panel.warnings(), 3);

    // An operation that gets here anyway (its dialog was answered after the transfer
    // started) is refused, and does not end the transfer's `busy` when it would finish.
    let ran = Rc::new(Cell::new(false));
    let operation = {
        let ran = ran.clone();
        async move {
            ran.set(true);
            Ok(())
        }
    };
    panel.update(cx, |panel, _, cx| panel.run(operation, cx));
    cx.run_until_parked();
    assert!(!ran.get());
    assert!(panel.update(cx, |panel, _, _| panel.busy));

    // So the next transfer cannot take `work` from the one in flight.
    panel.update(cx, |panel, window, cx| {
        panel.fetch_file(file("b.txt"), "/b.txt".into(), |_, _, _, _| {}, window, cx);
        assert!(panel.transfer.is_none());
    });
    assert_eq!(panel.warnings(), 5);

    release.try_send(()).unwrap();
    cx.run_until_parked();
    assert!(finished.get(), "the transfer in flight was cancelled");
    assert!(!panel.update(cx, |panel, _, _| panel.busy));
}
