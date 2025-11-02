use super::State;
use crate::routes::GetState;
use axum::{
    body::Bytes,
    extract::{WebSocketUpgrade, ws::Message},
    response::Response,
    routing::any,
};
use std::{pin::Pin, sync::Arc};
use utoipa_axum::router::OpenApiRouter;

pub async fn handle_ws(ws: WebSocketUpgrade, state: GetState) -> Response {
    ws.on_upgrade(move |socket| async move {
        let socket = Arc::new(tokio::sync::Mutex::new(socket));

        type ReturnType = dyn futures_util::Future<Output = Result<(), anyhow::Error>> + Send;
        let futures: [Pin<Box<ReturnType>>; 2] = [
            // Stats Listener
            Box::pin({
                let state = Arc::clone(&state);
                let socket = Arc::clone(&socket);

                async move {
                    loop {
                        let stats = state.stats_manager.get_stats().await;

                        socket
                            .lock()
                            .await
                            .send(Message::Text(
                                serde_json::to_string(&*stats).unwrap().into(),
                            ))
                            .await?;

                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                }
            }),
            // Pinger
            Box::pin(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;

                    socket
                        .lock()
                        .await
                        .send(Message::Ping(Bytes::from_static(&[1, 2, 3])))
                        .await?;
                }
            }),
        ];

        if let Err(err) = futures_util::future::try_join_all(futures).await {
            tracing::debug!("error while serving stats websocket: {:#?}", err);
        }
    })
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .route("/", any(handle_ws))
        .with_state(state.clone())
}
