//! Saved NETCONF and gNMI requests, per host or for every host of a
//! protocol (the logic of `server/src/routes/management-requests.ts`). The
//! store keeps them; this checks what the UI sends the way the route did.

use muxus_core::js::utf16_len;
use muxus_core::management::{
    ManagementProtocol, SavedManagementRequest, SavedManagementRequestInput,
};
use muxus_core::validate::{self, ValidationError};
use serde_json::Value;

/// Largest request (editor state, including any XML or JSON body) the library keeps.
pub const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;

/// Where saved requests live; the store implements it.
pub trait ManagementRequestStore {
    fn list(
        &self,
        protocol: ManagementProtocol,
        profile_id: Option<&str>,
    ) -> Result<Vec<SavedManagementRequest>, String>;
    fn save(&self, input: SavedManagementRequestInput) -> Result<SavedManagementRequest, String>;
    fn delete(&self, id: &str) -> Result<bool, String>;
}

/// Why the library refused, with the HTTP status the route answered with.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LibraryError {
    /// 400
    #[error("{0}")]
    Invalid(String),
    /// 413
    #[error("The request is too large to save.")]
    TooLarge,
    /// 500: the store failed.
    #[error("{0}")]
    Store(String),
}

impl LibraryError {
    pub fn status(&self) -> u16 {
        match self {
            LibraryError::Invalid(_) => 400,
            LibraryError::TooLarge => 413,
            LibraryError::Store(_) => 500,
        }
    }
}

fn invalid(err: ValidationError) -> LibraryError {
    LibraryError::Invalid(err.message)
}

/// `GET /api/management/requests?protocol=…&profileId=…`
pub fn list_requests(
    store: &dyn ManagementRequestStore,
    protocol: Option<&str>,
    profile_id: Option<&str>,
) -> Result<Vec<SavedManagementRequest>, LibraryError> {
    let protocol = protocol
        .and_then(ManagementProtocol::parse)
        .ok_or_else(|| LibraryError::Invalid("protocol must be gnmi or netconf".into()))?;
    store
        .list(protocol, profile_id.filter(|id| !id.is_empty()))
        .map_err(LibraryError::Store)
}

/// Check a body the way `savedRequestSchema` and the size limit do.
pub fn validate_saved_request(body: Value) -> Result<SavedManagementRequestInput, LibraryError> {
    let mut input: SavedManagementRequestInput = serde_json::from_value(body)
        .map_err(|err| LibraryError::Invalid(format!("Invalid input: {err}")))?;
    if let Some(id) = &input.id {
        validate::string("id", id, 1, 200).map_err(invalid)?;
    }
    if let Some(profile_id) = &input.profile_id {
        validate::string("profileId", profile_id, 1, 200).map_err(invalid)?;
    }
    validate::trimmed("name", &mut input.name, 1, 200).map_err(invalid)?;
    let text = serde_json::to_string(&input.request).unwrap_or_default();
    if utf16_len(&text) > MAX_REQUEST_BYTES {
        return Err(LibraryError::TooLarge);
    }
    Ok(input)
}

/// `PUT /api/management/requests`
pub fn save_request(
    store: &dyn ManagementRequestStore,
    body: Value,
) -> Result<SavedManagementRequest, LibraryError> {
    let input = validate_saved_request(body)?;
    store.save(input).map_err(LibraryError::Store)
}

/// `DELETE /api/management/requests/:id`
pub fn delete_request(store: &dyn ManagementRequestStore, id: &str) -> Result<bool, LibraryError> {
    store.delete(id).map_err(LibraryError::Store)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use serde_json::json;

    use super::*;

    #[derive(Default)]
    struct Memory(RefCell<Vec<SavedManagementRequest>>);

    impl ManagementRequestStore for Memory {
        fn list(
            &self,
            protocol: ManagementProtocol,
            profile_id: Option<&str>,
        ) -> Result<Vec<SavedManagementRequest>, String> {
            Ok(self
                .0
                .borrow()
                .iter()
                .filter(|saved| saved.protocol == protocol)
                .filter(|saved| {
                    saved.profile_id.is_none() || saved.profile_id.as_deref() == profile_id
                })
                .cloned()
                .collect())
        }

        fn save(
            &self,
            input: SavedManagementRequestInput,
        ) -> Result<SavedManagementRequest, String> {
            let saved = SavedManagementRequest {
                id: input.id.unwrap_or_else(|| "new".into()),
                protocol: input.protocol,
                profile_id: input.profile_id,
                name: input.name,
                request: input.request,
                created_at: "2026-10-12T00:00:00.000Z".into(),
                updated_at: "2026-10-12T00:00:00.000Z".into(),
            };
            self.0.borrow_mut().push(saved.clone());
            Ok(saved)
        }

        fn delete(&self, id: &str) -> Result<bool, String> {
            let before = self.0.borrow().len();
            self.0.borrow_mut().retain(|saved| saved.id != id);
            Ok(self.0.borrow().len() < before)
        }
    }

    #[test]
    fn saves_lists_and_deletes_with_the_routes_checks() {
        let store = Memory::default();
        let saved = save_request(
            &store,
            json!({"protocol": "gnmi", "name": "  interfaces ", "request": {"operation": "get"}}),
        )
        .unwrap();
        assert_eq!(saved.name, "interfaces");
        assert_eq!(
            list_requests(&store, Some("gnmi"), Some("")).unwrap().len(),
            1
        );
        assert_eq!(
            list_requests(&store, Some("ssh"), None).unwrap_err(),
            LibraryError::Invalid("protocol must be gnmi or netconf".into())
        );
        let blank = save_request(
            &store,
            json!({"protocol": "gnmi", "name": "  ", "request": {}}),
        );
        assert_eq!(blank.unwrap_err().status(), 400);
        let huge = save_request(
            &store,
            json!({"protocol": "netconf", "name": "big", "request": {"config": "x".repeat(MAX_REQUEST_BYTES)}}),
        );
        assert_eq!(huge.unwrap_err(), LibraryError::TooLarge);
        assert!(delete_request(&store, "new").unwrap());
        assert!(!delete_request(&store, "new").unwrap());
    }
}
