//! The gNOI and reflection exchanges the UI does not drive message by
//! message: file transfers (chunked, with the hash gNOI requires checked or
//! supplied here) and listing the services a device offers.

use std::time::Duration;

use bytes::Bytes;
use md5::Md5;
use prost::Message;
use prost_reflect::{DynamicMessage, MessageDescriptor, Value as ProtoValue};
use sha2::{Digest, Sha256, Sha512};
use tokio_util::sync::CancellationToken;

use crate::error::{GrpcError, code};
use crate::gnmi::{CallEvent, CallOptions, GnmiClient, GrpcCall};
use crate::proto::pool;

const CHUNK_BYTES: usize = 64 * 1024;
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Larger files go through the device's own tooling, not a tab.
pub const MAX_TRANSFER_BYTES: u64 = 256 * 1024 * 1024;

/// gnoi.types.HashType.HashMethod.
const SHA256: i32 = 1;
const SHA512: i32 = 2;
const MD5: i32 = 3;

fn message(name: &str) -> MessageDescriptor {
    pool()
        .get_message_by_name(name)
        .unwrap_or_else(|| panic!("{name} is in the vendored protos"))
}

fn encode(name: &str, build: impl FnOnce(&mut DynamicMessage)) -> Bytes {
    let mut message = DynamicMessage::new(self::message(name));
    build(&mut message);
    Bytes::from(message.encode_to_vec())
}

fn bytes_field(message: &DynamicMessage, name: &str) -> Option<Bytes> {
    match message.get_field_by_name(name)?.as_ref() {
        ProtoValue::Bytes(bytes) => Some(bytes.clone()),
        _ => None,
    }
}

/// Every hash a device may name, computed along the way: it says which one
/// only at the end.
#[derive(Default)]
struct Digests {
    sha256: Sha256,
    sha512: Sha512,
    md5: Md5,
}

impl Digests {
    fn update(&mut self, chunk: &[u8]) {
        self.sha256.update(chunk);
        self.sha512.update(chunk);
        self.md5.update(chunk);
    }

    fn finish(self, method: i32) -> Option<(&'static str, Vec<u8>)> {
        Some(match method {
            SHA256 => ("sha256", self.sha256.finalize().to_vec()),
            SHA512 => ("sha512", self.sha512.finalize().to_vec()),
            MD5 => ("md5", self.md5.finalize().to_vec()),
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadResult {
    pub size: u64,
    /// The device sent a hash and the content matched it.
    pub verified: bool,
    pub hash_method: Option<String>,
}

async fn next_event(
    call: &mut GrpcCall,
    cancel: Option<&CancellationToken>,
    cancelled: &mut bool,
) -> Option<CallEvent> {
    loop {
        match cancel {
            Some(cancel) if !*cancelled => {
                tokio::select! {
                    () = cancel.cancelled() => {
                        *cancelled = true;
                        call.handle.cancel(None);
                    }
                    event = call.next() => return event,
                }
            }
            _ => return call.next().await,
        }
    }
}

/// Stream a file from the device, handing each chunk on as it arrives and checking the hash at the end.
pub async fn download_file(
    client: &GnmiClient,
    remote_file: &str,
    mut on_data: impl FnMut(Bytes),
    cancel: Option<&CancellationToken>,
) -> Result<DownloadResult, GrpcError> {
    let mut call = client.open_call(
        "/gnoi.file.File/Get",
        CallOptions {
            timeout: Some(TRANSFER_TIMEOUT),
            max_bytes: Some(MAX_TRANSFER_BYTES),
        },
    );
    call.handle.write(encode("gnoi.file.GetRequest", |request| {
        request.set_field_by_name("remote_file", ProtoValue::String(remote_file.to_owned()));
    }));
    call.handle.end();
    let response_desc = message("gnoi.file.GetResponse");
    let mut digests = Digests::default();
    let mut size = 0u64;
    let mut hash: Option<(i32, Bytes)> = None;
    let mut cancelled = false;
    while let Some(event) = next_event(&mut call, cancel, &mut cancelled).await {
        match event {
            CallEvent::Message(bytes) => {
                let response = match DynamicMessage::decode(response_desc.clone(), bytes) {
                    Ok(response) => response,
                    Err(err) => {
                        call.handle.cancel(Some(GrpcError::new(
                            code::INTERNAL,
                            format!("Could not decode a reply: {err}"),
                        )));
                        continue;
                    }
                };
                if let Some(chunk) = bytes_field(&response, "contents")
                    && response.has_field_by_name("contents")
                {
                    size += chunk.len() as u64;
                    digests.update(&chunk);
                    on_data(chunk);
                }
                if response.has_field_by_name("hash")
                    && let Some(ProtoValue::Message(value)) =
                        response.get_field_by_name("hash").as_deref()
                {
                    let method = match value.get_field_by_name("method").as_deref() {
                        Some(ProtoValue::EnumNumber(method)) => *method,
                        _ => 0,
                    };
                    hash = Some((method, bytes_field(value, "hash").unwrap_or_default()));
                }
            }
            CallEvent::End(Some(error)) => return Err(error),
            CallEvent::End(None) => break,
        }
    }
    let Some((method, expected)) = hash else {
        return Ok(DownloadResult {
            size,
            verified: false,
            hash_method: None,
        });
    };
    let Some((algorithm, actual)) = digests.finish(method) else {
        return Ok(DownloadResult {
            size,
            verified: false,
            hash_method: None,
        });
    };
    if actual != expected.as_ref() {
        return Err(GrpcError::new(
            code::DATA_LOSS,
            format!(
                "The {} of the received file does not match the device's.",
                algorithm.to_uppercase()
            ),
        ));
    }
    Ok(DownloadResult {
        size,
        verified: true,
        hash_method: Some(algorithm.into()),
    })
}

/// An upload in progress: chunks go out as they come, the SHA-256 follows the last one.
pub struct FileUpload {
    call: GrpcCall,
    digest: Sha256,
    pub bytes: u64,
}

impl FileUpload {
    /// `permissions` are octal digits written as a decimal number (644).
    pub fn new(client: &GnmiClient, remote_file: &str, permissions: Option<u32>) -> Self {
        let call = client.open_call(
            "/gnoi.file.File/Put",
            CallOptions {
                timeout: Some(TRANSFER_TIMEOUT),
                max_bytes: None,
            },
        );
        let details = message("gnoi.file.PutRequest.Details");
        let mut open = DynamicMessage::new(details);
        open.set_field_by_name("remote_file", ProtoValue::String(remote_file.to_owned()));
        if let Some(permissions) = permissions {
            open.set_field_by_name("permissions", ProtoValue::U32(permissions));
        }
        call.handle.write(encode("gnoi.file.PutRequest", |request| {
            request.set_field_by_name("open", ProtoValue::Message(open));
        }));
        Self {
            call,
            digest: Sha256::new(),
            bytes: 0,
        }
    }

    /// Send content, waiting for HTTP/2 to take each piece.
    pub async fn write(&mut self, chunk: &[u8]) -> Result<(), GrpcError> {
        for piece in chunk.chunks(CHUNK_BYTES) {
            self.digest.update(piece);
            self.bytes += piece.len() as u64;
            let message = encode("gnoi.file.PutRequest", |request| {
                request.set_field_by_name(
                    "contents",
                    ProtoValue::Bytes(Bytes::copy_from_slice(piece)),
                );
            });
            if let Err(err) = self.call.handle.write_drained(message).await {
                return Err(self.failure().await.unwrap_or(err));
            }
        }
        Ok(())
    }

    /// The call's own failure, if it already ended with one.
    async fn failure(&mut self) -> Option<GrpcError> {
        let ended = tokio::time::timeout(Duration::from_millis(200), self.call.next()).await;
        match ended {
            Ok(Some(CallEvent::End(Some(error)))) => Some(error),
            _ => None,
        }
    }

    /// Send the hash, end the upload and wait for the device to accept it.
    pub async fn finish(mut self) -> Result<u64, GrpcError> {
        let hash_type = message("gnoi.types.HashType");
        let mut hash = DynamicMessage::new(hash_type);
        hash.set_field_by_name("method", ProtoValue::EnumNumber(SHA256));
        hash.set_field_by_name(
            "hash",
            ProtoValue::Bytes(Bytes::from(self.digest.finalize_reset().to_vec())),
        );
        self.call
            .handle
            .write(encode("gnoi.file.PutRequest", |request| {
                request.set_field_by_name("hash", ProtoValue::Message(hash));
            }));
        self.call.handle.end();
        while let Some(event) = self.call.next().await {
            match event {
                CallEvent::Message(_) => {}
                CallEvent::End(Some(error)) => return Err(error),
                CallEvent::End(None) => break,
            }
        }
        Ok(self.bytes)
    }

    pub fn cancel(&self) {
        self.call.handle.cancel(None);
    }

    /// A handle that can cancel the upload from elsewhere.
    pub fn canceller(&self) -> crate::gnmi::CallHandle {
        self.call.handle.clone()
    }
}

const REFLECTION_PATHS: [(&str, &str); 2] = [
    (
        "/grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo",
        "grpc.reflection.v1alpha",
    ),
    (
        "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo",
        "grpc.reflection.v1",
    ),
];

/// Service names from gRPC server reflection (v1alpha, then v1), or `None`
/// when the device does not offer reflection.
pub async fn list_services(client: &GnmiClient) -> Option<Vec<String>> {
    for (path, package) in REFLECTION_PATHS {
        let mut call = client.open_call(
            path,
            CallOptions {
                timeout: Some(Duration::from_secs(10)),
                max_bytes: None,
            },
        );
        call.handle.write(encode(
            &format!("{package}.ServerReflectionRequest"),
            |request| {
                request.set_field_by_name("list_services", ProtoValue::String("*".into()));
            },
        ));
        let response_desc = message(&format!("{package}.ServerReflectionResponse"));
        let mut services: Option<Vec<String>> = None;
        let mut error = None;
        while let Some(event) = call.next().await {
            match event {
                CallEvent::Message(bytes) => {
                    if let Ok(response) = DynamicMessage::decode(response_desc.clone(), bytes)
                        && response.has_field_by_name("list_services_response")
                        && let Some(ProtoValue::Message(list)) = response
                            .get_field_by_name("list_services_response")
                            .as_deref()
                    {
                        let names = match list.get_field_by_name("service").as_deref() {
                            Some(ProtoValue::List(items)) => items
                                .iter()
                                .filter_map(|item| match item {
                                    ProtoValue::Message(service) => {
                                        match service.get_field_by_name("name").as_deref() {
                                            Some(ProtoValue::String(name)) if !name.is_empty() => {
                                                Some(name.clone())
                                            }
                                            _ => None,
                                        }
                                    }
                                    _ => None,
                                })
                                .collect(),
                            _ => Vec::new(),
                        };
                        services = Some(names);
                    }
                    call.handle.end();
                }
                CallEvent::End(end) => {
                    error = end;
                    break;
                }
            }
        }
        if let Some(mut services) = services {
            services.sort();
            return Some(services);
        }
        if let Some(error) = error
            && error.code != code::UNIMPLEMENTED
        {
            return None;
        }
    }
    None
}
