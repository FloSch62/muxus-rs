//! The route half of `tests/unit/server/environments.test.ts` (`environment routes`): every
//! route is a `Backend` method; the HTTP status of a failure is on `ApiError`.

mod support;

use muxus_core::environments::{
    ContainerAction, ContainerRuntime, EnvironmentAction, EnvironmentPortRequest,
};
use muxus_session::environments::EnvironmentSource;
use support::start;

#[tokio::test]
async fn needs_a_live_connection() {
    let sandbox = start().await;
    let error = sandbox
        .backend
        .environments(EnvironmentSource::ssh("nope"), false)
        .await
        .unwrap_err();
    assert_eq!(error.status, 404);
    let port = sandbox
        .backend
        .environment_port(
            "nope",
            EnvironmentPortRequest {
                host: "127.0.0.1".into(),
                port: 8080,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(port.status, 404);
    sandbox.stop().await;
}

#[tokio::test]
async fn validates_names_before_running_anything() {
    let sandbox = start().await;
    let pods = sandbox
        .backend
        .environment_pods(EnvironmentSource::Local, None, Some("Not_Valid".into()))
        .await
        .unwrap_err();
    assert_eq!(pods.status, 400);
    let action = sandbox
        .backend
        .environment_action(
            EnvironmentSource::Local,
            EnvironmentAction::Container {
                runtime: ContainerRuntime::Docker,
                container: "--rm".into(),
                action: ContainerAction::Stop,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(action.status, 400);
    let port = sandbox
        .backend
        .environment_port(
            "any",
            EnvironmentPortRequest {
                host: "-oProxyCommand=x".into(),
                port: 22,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(port.status, 400);
    sandbox.stop().await;
}
