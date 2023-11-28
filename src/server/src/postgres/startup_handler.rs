use std::fmt::Debug;
use std::sync::Arc;

use async_trait::async_trait;
use futures::sink::{Sink, SinkExt};
use pgwire::api::auth::{AuthSource, LoginInfo, ServerParameterProvider, StartupHandler};
use pgwire::api::{ClientInfo, PgWireConnectionState};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use pgwire::messages::response::ErrorResponse;
use pgwire::messages::startup::Authentication;
use pgwire::messages::{PgWireBackendMessage, PgWireFrontendMessage};
use tokio::sync::Mutex;

pub struct DataClodStartupHandler<A, P> {
    pub auth_source: Arc<A>,
    pub parameter_provider: Arc<P>,
    pub cached_password: Mutex<Vec<u8>>,
}

#[async_trait]
impl<A: AuthSource, P: ServerParameterProvider> StartupHandler for DataClodStartupHandler<A, P> {
    async fn on_startup<C>(
        &self, client: &mut C, message: PgWireFrontendMessage,
    ) -> PgWireResult<()>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        match message {
            PgWireFrontendMessage::Startup(ref startup) => {
                pgwire::api::auth::save_startup_parameters_to_metadata(client, startup);
                client.set_state(PgWireConnectionState::AuthenticationInProgress);

                let login_info = LoginInfo::from_client_info(client);
                let salt_and_pass = self.auth_source.get_password(&login_info).await?;

                let salt = salt_and_pass
                    .salt()
                    .as_ref()
                    .expect("Salt is required for Md5Password authentication");

                *self.cached_password.lock().await = salt_and_pass.password().clone();

                client
                    .send(PgWireBackendMessage::Authentication(
                        Authentication::MD5Password(salt.clone()),
                    ))
                    .await?;
            }
            PgWireFrontendMessage::PasswordMessageFamily(pwd) => {
                let pwd = pwd.into_password()?;
                let cached_pass = self.cached_password.lock().await;

                let login_info = LoginInfo::from_client_info(client);
                if pwd.password().as_bytes() == *cached_pass
                    && login_info.user().as_deref() == Some(&"postgres".to_string())
                {
                    pgwire::api::auth::finish_authentication(
                        client,
                        self.parameter_provider.as_ref(),
                    )
                    .await
                } else {
                    let error_info = ErrorInfo::new(
                        "FATAL".to_owned(),
                        "28P01".to_owned(),
                        "Password authentication failed".to_owned(),
                    );
                    let error = ErrorResponse::from(error_info);

                    client
                        .feed(PgWireBackendMessage::ErrorResponse(error))
                        .await?;
                    client.close().await?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}
