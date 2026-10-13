//! Port of tests/unit/server/sftp-lease-socket.test.ts (detached SFTP window leases).

use super::*;
use crate::Problem;
use crate::sftp::SftpWindowLease;

#[tokio::test]
async fn holds_the_transport_until_the_window_closes() {
    let mut fx = Fixture::new().await;
    let window = fx.service.hold_window(&fx.conn_id).unwrap();
    assert_eq!(window.connection_id(), fx.conn_id);
    assert_eq!(window.owner(), ConnectionLeaseOwner::Sftp);
    assert_eq!(fx.sftp_leases(), 1);

    // The terminal tab closes; the window keeps the shared transport alive.
    drop(fx.terminal.take());
    let connection = fx.manager.get(&fx.conn_id).unwrap();
    assert!(!connection.is_closed());

    // Closing the window releases the last lease, and the transport goes with it.
    drop(window);
    assert!(fx.manager.get(&fx.conn_id).is_none());
    assert!(eventually(|| connection.is_closed()).await);
}

#[tokio::test]
async fn releasing_twice_releases_once() {
    let fx = Fixture::new().await;
    let window = fx.service.hold_window(&fx.conn_id).unwrap();
    let editor =
        SftpWindowLease::acquire_as(&fx.manager, &fx.conn_id, ConnectionLeaseOwner::Editor)
            .unwrap();
    window.release();
    window.release();
    assert_eq!(fx.sftp_leases(), 0);
    assert_eq!(
        fx.manager
            .lease_count(&fx.conn_id, Some(&[ConnectionLeaseOwner::Editor])),
        1
    );
    drop(window);
    drop(editor);
    assert!(fx.manager.get(&fx.conn_id).is_some());
}

#[tokio::test]
async fn rejects_a_stale_connection_id() {
    let fx = Fixture::new().await;
    assert_eq!(
        fx.service.hold_window("stale").unwrap_err(),
        Problem::not_found("connection not found")
    );
    assert_eq!(fx.sftp_leases(), 0);
}

#[tokio::test]
async fn lets_go_when_the_connection_closes_underneath_the_window() {
    let fx = Fixture::new().await;
    let window = fx.service.hold_window(&fx.conn_id).unwrap();
    let connection = fx.manager.get(&fx.conn_id).unwrap();

    connection.close();
    tokio::time::timeout(Duration::from_secs(10), window.closed())
        .await
        .expect("the window hears about the closed connection");

    assert_eq!(fx.sftp_leases(), 0);
    assert!(fx.service.hold_window(&fx.conn_id).is_err());
}
