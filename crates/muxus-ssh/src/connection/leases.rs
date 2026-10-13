//! Owns transport lifetimes independently from UI/channel lifetimes.
//!
//! Every terminal, SFTP operation, forward, or editor holds a lease. Closing one consumer
//! releases only that lease; the underlying transport closes after the last consumer leaves or
//! when `close_all()` forces shutdown.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Who holds a lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectionLeaseOwner {
    Terminal,
    Sftp,
    Forward,
    Editor,
    Dial,
    Desktop,
    Management,
    Stats,
}

/// A transport the registry can close.
pub trait LeaseableConnection: Send + Sync {
    fn id(&self) -> &str;
    fn close(&self);
}

struct Record<T> {
    connection: Arc<T>,
    leases: HashMap<u64, ConnectionLeaseOwner>,
    next_lease_id: u64,
    closing: bool,
}

struct Inner<T> {
    records: Vec<(String, Record<T>)>,
}

impl<T> Inner<T> {
    fn record(&mut self, id: &str) -> Option<&mut Record<T>> {
        self.records
            .iter_mut()
            .find(|(rid, _)| rid == id)
            .map(|(_, r)| r)
    }
}

/// Live transports and their leases.
pub struct ConnectionLeaseRegistry<T: LeaseableConnection> {
    inner: Arc<Mutex<Inner<T>>>,
}

impl<T: LeaseableConnection> Default for ConnectionLeaseRegistry<T> {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                records: Vec::new(),
            })),
        }
    }
}

impl<T: LeaseableConnection + 'static> ConnectionLeaseRegistry<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a fresh transport with its first lease.
    pub fn register(&self, connection: Arc<T>, owner: ConnectionLeaseOwner) -> TransportLease<T> {
        let id = connection.id().to_string();
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            assert!(
                inner.record(&id).is_none(),
                "connection \"{id}\" is already registered"
            );
            inner.records.push((
                id.clone(),
                Record {
                    connection,
                    leases: HashMap::new(),
                    next_lease_id: 1,
                    closing: false,
                },
            ));
        }
        self.acquire(&id, owner).expect("just registered")
    }

    pub fn get(&self, id: &str) -> Option<Arc<T>> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .record(id)
            .filter(|r| !r.closing)
            .map(|r| r.connection.clone())
    }

    /// All live (non-closing) transports, registration order.
    pub fn list(&self) -> Vec<Arc<T>> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .records
            .iter()
            .filter(|(_, r)| !r.closing)
            .map(|(_, r)| r.connection.clone())
            .collect()
    }

    /// Live leases on `id`, optionally counting only the given owner kinds.
    pub fn lease_count(&self, id: &str, owners: Option<&[ConnectionLeaseOwner]>) -> usize {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(record) = inner.record(id).filter(|r| !r.closing) else {
            return 0;
        };
        match owners {
            None => record.leases.len(),
            Some(owners) => record
                .leases
                .values()
                .filter(|o| owners.contains(o))
                .count(),
        }
    }

    pub fn acquire(&self, id: &str, owner: ConnectionLeaseOwner) -> Option<TransportLease<T>> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let record = inner.record(id).filter(|r| !r.closing)?;
        let lease_id = record.next_lease_id;
        record.next_lease_id += 1;
        record.leases.insert(lease_id, owner);
        Some(TransportLease {
            connection: record.connection.clone(),
            owner,
            registry: self.inner.clone(),
            id: id.to_string(),
            lease_id,
            released: AtomicBool::new(false),
        })
    }

    /// Record an already-closed transport and invalidate all outstanding leases.
    pub fn mark_closed(&self, connection: &T) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let id = connection.id();
        if let Some(pos) = inner
            .records
            .iter()
            .position(|(rid, r)| rid == id && std::ptr::eq(Arc::as_ptr(&r.connection), connection))
        {
            inner.records.remove(pos);
        }
    }

    /// Force-closes every registered transport (application shutdown).
    pub fn close_all(&self) {
        let records: Vec<Arc<T>> = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.records.drain(..).map(|(_, r)| r.connection).collect()
        };
        for connection in records {
            connection.close();
        }
    }
}

/// One consumer's hold on a transport. Released explicitly or on drop; idempotent.
pub struct TransportLease<T: LeaseableConnection> {
    connection: Arc<T>,
    owner: ConnectionLeaseOwner,
    registry: Arc<Mutex<Inner<T>>>,
    id: String,
    lease_id: u64,
    released: AtomicBool,
}

impl<T: LeaseableConnection> std::fmt::Debug for TransportLease<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransportLease")
            .field("id", &self.id)
            .field("owner", &self.owner)
            .finish()
    }
}

impl<T: LeaseableConnection> TransportLease<T> {
    pub fn connection(&self) -> &Arc<T> {
        &self.connection
    }

    pub fn owner(&self) -> ConnectionLeaseOwner {
        self.owner
    }

    /// The transport closes after its final lease is released.
    pub fn release(&self) {
        if self.released.swap(true, Ordering::SeqCst) {
            return;
        }
        let close = {
            let mut inner = self.registry.lock().unwrap_or_else(|e| e.into_inner());
            match inner.record(&self.id) {
                Some(record) if Arc::ptr_eq(&record.connection, &self.connection) => {
                    let removed = record.leases.remove(&self.lease_id).is_some();
                    if removed && !record.closing && record.leases.is_empty() {
                        record.closing = true;
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            }
        };
        if close {
            self.connection.close();
        }
    }
}

impl<T: LeaseableConnection> Drop for TransportLease<T> {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct Conn {
        id: String,
        closes: AtomicUsize,
    }

    impl LeaseableConnection for Conn {
        fn id(&self) -> &str {
            &self.id
        }
        fn close(&self) {
            self.closes.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn connection(id: &str) -> Arc<Conn> {
        Arc::new(Conn {
            id: id.into(),
            closes: AtomicUsize::new(0),
        })
    }

    fn closes(c: &Conn) -> usize {
        c.closes.load(Ordering::SeqCst)
    }

    use ConnectionLeaseOwner::*;

    #[test]
    fn keeps_a_transport_alive_until_its_final_independent_consumer_releases_it() {
        let registry = ConnectionLeaseRegistry::new();
        let transport = connection("connection-1");
        let terminal = registry.register(transport.clone(), Terminal);
        let forward = registry.acquire("connection-1", Forward).unwrap();
        let sftp = registry.acquire("connection-1", Sftp).unwrap();

        terminal.release();
        sftp.release();
        assert_eq!(closes(&transport), 0);
        assert!(Arc::ptr_eq(
            &registry.get("connection-1").unwrap(),
            &transport
        ));

        forward.release();
        assert_eq!(closes(&transport), 1);
        assert!(registry.acquire("connection-1", Editor).is_none());

        forward.release();
        assert_eq!(closes(&transport), 1);
    }

    #[test]
    fn invalidates_leases_when_the_remote_transport_closes_first() {
        let registry = ConnectionLeaseRegistry::new();
        let transport = connection("connection-1");
        let terminal = registry.register(transport.clone(), Terminal);
        let forward = registry.acquire("connection-1", Forward).unwrap();

        registry.mark_closed(&transport);
        terminal.release();
        forward.release();

        assert!(registry.get("connection-1").is_none());
        assert_eq!(closes(&transport), 0);
    }

    #[test]
    fn supports_the_tunnel_start_handover() {
        let registry = ConnectionLeaseRegistry::new();
        let transport = connection("connection-1");
        let dial = registry.register(transport.clone(), Dial);
        let forward = registry.acquire("connection-1", Forward).unwrap();

        // The dial socket closes once the forward is up; the tunnel survives.
        dial.release();
        assert_eq!(closes(&transport), 0);
        assert_eq!(registry.list().len(), 1);

        // Stopping the tunnel releases the final lease and the transport closes.
        drop(forward);
        assert_eq!(closes(&transport), 1);
        assert!(registry.list().is_empty());
    }

    #[test]
    fn counts_live_leases_optionally_by_owner_kind() {
        let registry = ConnectionLeaseRegistry::new();
        let transport = connection("connection-1");
        let terminal = registry.register(transport.clone(), Terminal);
        let _second = registry.acquire("connection-1", Terminal).unwrap();
        let forward = registry.acquire("connection-1", Forward).unwrap();

        assert_eq!(registry.lease_count("connection-1", None), 3);
        assert_eq!(
            registry.lease_count("connection-1", Some(&[Terminal, Dial])),
            2
        );
        assert_eq!(registry.lease_count("connection-1", Some(&[Sftp])), 0);
        assert_eq!(registry.lease_count("missing", None), 0);

        terminal.release();
        assert_eq!(
            registry.lease_count("connection-1", Some(&[Terminal, Dial])),
            1
        );

        registry.mark_closed(&transport);
        assert_eq!(registry.lease_count("connection-1", None), 0);
        forward.release();
    }

    #[test]
    fn force_closes_every_registered_transport_during_application_shutdown() {
        let registry = ConnectionLeaseRegistry::new();
        let (first, second) = (connection("first"), connection("second"));
        let _a = registry.register(first.clone(), Terminal);
        let _b = registry.register(second.clone(), Forward);

        registry.close_all();

        assert_eq!(closes(&first), 1);
        assert_eq!(closes(&second), 1);
        assert!(registry.get("first").is_none());
        assert!(registry.get("second").is_none());
        // Dropping leases on a closed registry closes nothing twice.
        drop(_a);
        assert_eq!(closes(&first), 1);
    }
}
