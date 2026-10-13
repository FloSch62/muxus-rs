//! The fake gNMI device of the workbench example on its own, for reference
//! captures of the Electron build: `cargo run --example management_device -- 57400`.

#[path = "support/device.rs"]
mod device;

#[tokio::main]
async fn main() {
    let port = std::env::args()
        .nth(1)
        .and_then(|port| port.parse().ok())
        .unwrap_or(57400);
    let port = device::Device::new().listen(port).await;
    println!("fake gNMI device on 127.0.0.1:{port}");
    std::future::pending::<()>().await;
}
