//! gNOI file transfers and gRPC reflection against a fake target (port of
//! `tests/unit/server/gnoi-services.test.ts`; the JSON mapping tests live
//! next to `services::json`).

mod common;

use bytes::Bytes;
use muxus_mgmt::error::code;
use muxus_mgmt::gnmi::GnmiClient;
use muxus_mgmt::proto::pool;
use muxus_mgmt::services::json::{decode_json, encode_json};
use muxus_mgmt::services::transfer::{DownloadResult, FileUpload, download_file, list_services};
use serde_json::{Value, json};
use sha2::{Digest, Sha256, Sha512};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

fn encode(message: &str, value: Value) -> Vec<u8> {
    encode_json(&pool().get_message_by_name(message).unwrap(), &value).unwrap()
}

fn decode(message: &str, bytes: &[u8]) -> Value {
    Value::Object(decode_json(&pool().get_message_by_name(message).unwrap(), bytes).unwrap())
}

async fn target<F, Fut>(handle: F) -> GnmiClient
where
    F: Fn(common::Call) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let (socket, port) = common::plaintext_target(handle).await;
    GnmiClient::connect(socket, &format!("leaf1:{port}"), false)
        .await
        .unwrap()
}

#[tokio::test]
async fn downloads_a_file_and_checks_it_against_the_hash_the_device_sends() {
    let parts: Vec<Vec<u8>> = vec![vec![1; 70_000], b"tail".to_vec()];
    let whole: Vec<u8> = parts.concat();
    let paths = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = paths.clone();
    let client = target(move |mut call| {
        let parts = parts.clone();
        let whole = parts.concat();
        seen.lock().unwrap().push(call.path.clone());
        async move {
            call.ended().await;
            let mut messages: Vec<Vec<u8>> = parts
                .iter()
                .map(|contents| encode("gnoi.file.GetResponse", json!({"contents": BASE64.encode(contents)})))
                .collect();
            messages.push(encode(
                "gnoi.file.GetResponse",
                json!({"hash": {"method": "SHA512", "hash": BASE64.encode(Sha512::digest(&whole))}}),
            ));
            let refs: Vec<&[u8]> = messages.iter().map(Vec::as_slice).collect();
            call.reply(&refs, 0, None);
        }
    })
    .await;
    let mut received = Vec::new();
    let result = download_file(
        &client,
        "/tmp/x",
        |chunk: Bytes| received.extend_from_slice(&chunk),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        result,
        DownloadResult {
            size: whole.len() as u64,
            verified: true,
            hash_method: Some("sha512".into())
        }
    );
    assert_eq!(received, whole);
    assert_eq!(paths.lock().unwrap()[0], "/gnoi.file.File/Get");
}

#[tokio::test]
async fn fails_a_download_whose_content_does_not_match_the_hash() {
    let client = target(|mut call| async move {
        call.ended().await;
        let corrupted = encode("gnoi.file.GetResponse", json!({"contents": BASE64.encode("corrupted")}));
        let hash = encode(
            "gnoi.file.GetResponse",
            json!({"hash": {"method": "SHA256", "hash": BASE64.encode(Sha256::digest(b"original"))}}),
        );
        call.reply(&[&corrupted, &hash], 0, None);
    })
    .await;
    let error = download_file(&client, "/tmp/x", |_| {}, None)
        .await
        .unwrap_err();
    assert_eq!(error.code, code::DATA_LOSS);
    assert_eq!(
        error.message,
        "The SHA256 of the received file does not match the device's."
    );
}

#[tokio::test]
async fn uploads_in_chunks_and_ends_with_the_sha256_of_what_it_sent() {
    let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let keep = received.clone();
    let client = target(move |mut call| {
        let keep = keep.clone();
        async move {
            call.ended().await;
            *keep.lock().unwrap() = call.received.lock().unwrap().clone();
            call.reply(&[&encode("gnoi.file.PutResponse", json!({}))], 0, None);
        }
    })
    .await;
    let content = vec![7u8; 150_000];
    let mut upload = FileUpload::new(&client, "/tmp/up.bin", Some(644));
    upload.write(&content[..100_000]).await.unwrap();
    upload.write(&content[100_000..]).await.unwrap();
    assert_eq!(upload.finish().await.unwrap(), content.len() as u64);

    let requests: Vec<Value> = received
        .lock()
        .unwrap()
        .iter()
        .map(|message| decode("gnoi.file.PutRequest", message))
        .collect();
    assert_eq!(
        requests[0],
        json!({"open": {"remote_file": "/tmp/up.bin", "permissions": 644}})
    );
    let chunks: Vec<Vec<u8>> = requests[1..requests.len() - 1]
        .iter()
        .map(|request| {
            BASE64
                .decode(request["contents"].as_str().unwrap())
                .unwrap()
        })
        .collect();
    assert!(chunks.iter().all(|chunk| chunk.len() <= 64 * 1024));
    assert_eq!(chunks.concat(), content);
    assert_eq!(
        requests.last().unwrap(),
        &json!({"hash": {"method": "SHA256", "hash": BASE64.encode(Sha256::digest(&content))}})
    );
}

#[tokio::test]
async fn reports_an_upload_the_device_refuses() {
    let client = target(|mut call| async move {
        let _open = call.next_request().await;
        call.finish(code::PERMISSION_DENIED, Some("read-only file system"));
    })
    .await;
    let mut upload = FileUpload::new(&client, "cf3:\\x.cfg", Some(777));
    let written = upload.write(&vec![1; 300_000]).await;
    let error = match written {
        Err(error) => error,
        Ok(()) => upload.finish().await.unwrap_err(),
    };
    assert_eq!(error.code_name(), "PERMISSION_DENIED");
}

#[tokio::test]
async fn falls_back_to_v1_reflection_when_the_device_lacks_v1alpha() {
    let paths = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (seen, keep) = (paths.clone(), requests.clone());
    let client = target(move |mut call| {
        seen.lock().unwrap().push(call.path.clone());
        let keep = keep.clone();
        async move {
            if call.path.contains("v1alpha") {
                call.reply(&[], code::UNIMPLEMENTED, Some("unknown service"));
                return;
            }
            let request = call.next_request().await.unwrap();
            keep.lock().unwrap().push(request);
            let response = encode(
                "grpc.reflection.v1.ServerReflectionResponse",
                json!({"list_services_response": {"service": [{"name": "gnoi.system.System"}, {"name": "gnmi.gNMI"}]}}),
            );
            call.reply(&[&response], 0, None);
        }
    })
    .await;
    assert_eq!(
        list_services(&client).await,
        Some(vec![
            "gnmi.gNMI".to_owned(),
            "gnoi.system.System".to_owned()
        ])
    );
    assert_eq!(
        *paths.lock().unwrap(),
        vec![
            "/grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo",
            "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo",
        ]
    );
    assert_eq!(
        decode(
            "grpc.reflection.v1.ServerReflectionRequest",
            &requests.lock().unwrap()[0]
        ),
        json!({"list_services": "*"})
    );
}

#[tokio::test]
async fn reports_nothing_when_the_device_offers_no_reflection() {
    let client = target(|mut call| async move {
        call.reply(&[], code::UNIMPLEMENTED, None);
    })
    .await;
    assert_eq!(list_services(&client).await, None);
}
