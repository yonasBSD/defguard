pub(crate) mod client;

use std::{
    str::FromStr,
    sync::{Arc, Mutex},
};

use defguard_core::{
    auth::failed_login::FailedLoginMap,
    build_webapp,
    config::DefGuardConfig,
    db::{
        init_db, models::settings::initialize_current_settings, AppEvent, GatewayEvent, Id, NoId,
        User, UserDetails,
    },
    enterprise::license::{set_cached_license, License},
    events::ApiEvent,
    grpc::{GatewayMap, WorkerState},
    handlers::Auth,
    mail::Mail,
    SERVER_CONFIG,
};
use reqwest::{header::HeaderName, StatusCode, Url};
use secrecy::ExposeSecret;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use sqlx::{
    postgres::{PgConnectOptions, PgPoolOptions},
    query,
    types::Uuid,
    PgPool,
};
use tokio::{
    net::TcpListener,
    sync::{
        broadcast::{self, Receiver},
        mpsc::{unbounded_channel, UnboundedReceiver},
    },
};

use self::client::TestClient;

#[allow(clippy::declare_interior_mutable_const)]
pub const X_FORWARDED_HOST: HeaderName = HeaderName::from_static("x-forwarded-host");
#[allow(clippy::declare_interior_mutable_const)]
pub const X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");
#[allow(clippy::declare_interior_mutable_const)]
pub const X_FORWARDED_URI: HeaderName = HeaderName::from_static("x-forwarded-uri");

/// Allows overriding the default DefGuard URL for tests, as during the tests, the server has a random port, making the URL unpredictable beforehand.
// TODO: Allow customizing the whole config, not just the URL
pub(crate) fn init_config(custom_defguard_url: Option<&str>) -> DefGuardConfig {
    let url = custom_defguard_url.unwrap_or("http://localhost:8000");
    let mut config = DefGuardConfig::new_test_config();
    config.url = Url::from_str(url).unwrap();
    let _ = SERVER_CONFIG.set(config.clone());
    config
}

pub(crate) async fn init_test_db(config: &DefGuardConfig) -> PgPool {
    let opts = PgConnectOptions::new()
        .host(&config.database_host)
        .port(config.database_port)
        .username(&config.database_user)
        .password(config.database_password.expose_secret())
        .database(&config.database_name);
    let pool = PgPool::connect_with(opts)
        .await
        .expect("Failed to connect to Postgres");
    let db_name = Uuid::new_v4().to_string();
    query(&format!("CREATE DATABASE \"{db_name}\""))
        .execute(&pool)
        .await
        .expect("Failed to create test database");
    let pool = init_db(
        &config.database_host,
        config.database_port,
        &db_name,
        &config.database_user,
        config.database_password.expose_secret(),
    )
    .await;

    initialize_users(&pool, config).await;

    pool
}

pub(crate) async fn initialize_users(pool: &PgPool, config: &DefGuardConfig) {
    User::init_admin_user(pool, config.default_admin_password.expose_secret())
        .await
        .unwrap();

    User::new(
        "hpotter",
        Some("pass123"),
        "Potter",
        "Harry",
        "h.potter@hogwart.edu.uk",
        None,
    )
    .save(pool)
    .await
    .unwrap();
}

pub(crate) struct ClientState {
    pub pool: PgPool,
    pub worker_state: Arc<Mutex<WorkerState>>,
    pub wireguard_rx: Receiver<GatewayEvent>,
    pub mail_rx: UnboundedReceiver<Mail>,
    pub test_user: User<Id>,
    pub config: DefGuardConfig,
}

impl ClientState {
    pub fn new(
        pool: PgPool,
        worker_state: Arc<Mutex<WorkerState>>,
        wireguard_rx: Receiver<GatewayEvent>,
        mail_rx: UnboundedReceiver<Mail>,
        test_user: User<Id>,
        config: DefGuardConfig,
    ) -> Self {
        Self {
            pool,
            worker_state,
            wireguard_rx,
            mail_rx,
            test_user,
            config,
        }
    }
}

// Helper function to instantiate pool manually as a workaround for issues with `sqlx::test` macro
// reference: https://github.com/launchbadge/sqlx/issues/2567#issuecomment-2009849261
pub async fn setup_pool(options: PgConnectOptions) -> PgPool {
    PgPoolOptions::new().connect_with(options).await.unwrap()
}

pub(crate) async fn make_base_client(
    pool: PgPool,
    config: DefGuardConfig,
    listener: TcpListener,
) -> (TestClient, ClientState) {
    let (api_event_tx, api_event_rx) = unbounded_channel::<ApiEvent>();
    let (tx, rx) = unbounded_channel::<AppEvent>();
    let worker_state = Arc::new(Mutex::new(WorkerState::new(tx.clone())));
    let (wg_tx, wg_rx) = broadcast::channel::<GatewayEvent>(16);
    let (mail_tx, mail_rx) = unbounded_channel::<Mail>();
    let gateway_state = Arc::new(Mutex::new(GatewayMap::new()));

    let failed_logins = FailedLoginMap::new();
    let failed_logins = Arc::new(Mutex::new(failed_logins));

    let license = License::new(
        "test_customer".to_string(),
        false,
        // Permanent license
        None,
        None,
    );

    set_cached_license(Some(license));

    let client_state = ClientState::new(
        pool.clone(),
        worker_state.clone(),
        wg_rx,
        mail_rx,
        User::find_by_username(&pool, "hpotter")
            .await
            .unwrap()
            .unwrap(),
        config.clone(),
    );

    // Uncomment this to enable tracing in tests.
    // It only works for running a single test, so leave it commented out for running all tests.
    // use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
    // tracing_subscriber::registry()
    //     .with(
    //         tracing_subscriber::EnvFilter::try_from_default_env()
    //             .unwrap_or_else(|_| "defguard=debug,tower_http=debug,axum::rejection=trace".into()),
    //     )
    //     .with(tracing_subscriber::fmt::layer())
    //     .init();

    let webapp = build_webapp(
        tx,
        rx,
        wg_tx,
        mail_tx,
        worker_state,
        gateway_state,
        pool,
        failed_logins,
        api_event_tx,
    );

    (
        TestClient::new(webapp, listener, api_event_rx),
        client_state,
    )
}

/// Make an instance url based on the listener
fn get_test_url(listener: &TcpListener) -> String {
    let port = listener.local_addr().unwrap().port();
    format!("http://localhost:{port}")
}

pub(crate) async fn make_test_client(pool: PgPool) -> (TestClient, ClientState) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Could not bind ephemeral socket");
    let config = init_config(None);
    initialize_users(&pool, &config).await;
    initialize_current_settings(&pool)
        .await
        .expect("Could not initialize settings");
    make_base_client(pool, config, listener).await
}

/// Makes a test client with a DEFGUARD_URL set to the random url of the listener.
/// This is useful when the instance's url real url needs to match the one set in the ENV variable.
#[allow(dead_code)]
pub(crate) async fn make_test_client_with_real_url() -> (TestClient, ClientState) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Could not bind ephemeral socket");
    let config = init_config(Some(&get_test_url(&listener)));
    let pool = init_test_db(&config).await;
    make_base_client(pool, config, listener).await
}

pub(crate) async fn fetch_user_details(client: &TestClient, username: &str) -> UserDetails {
    let response = client.get(format!("/api/v1/user/{username}")).send().await;
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await
}

/// Exceeds enterprise free version limits by creating more than 1 network
pub(crate) async fn exceed_enterprise_limits(client: &TestClient) {
    let auth = Auth::new("admin", "pass123");
    client.post("/api/v1/auth").json(&auth).send().await;

    let response = client
        .post("/api/v1/network")
        .json(&json!({
            "name": "network1",
            "address": "10.1.1.1/24",
            "port": 55555,
            "endpoint": "192.168.4.14",
            "allowed_ips": "10.1.1.0/24",
            "dns": "1.1.1.1",
            "allowed_groups": [],
            "mfa_enabled": false,
            "keepalive_interval": 25,
            "peer_disconnect_threshold": 180,
            "acl_enabled": false,
            "acl_default_allow": false
        }))
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let response = client
        .post("/api/v1/network")
        .json(&json!({
                "name": "network2",
                "address": "10.1.1.1/24",
                "port": 55555,
                "endpoint": "192.168.4.14",
                "allowed_ips": "10.1.1.0/24",
                "dns": "1.1.1.1",
                "allowed_groups": [],
                "mfa_enabled": false,
                "keepalive_interval": 25,
                "peer_disconnect_threshold": 180,
                "acl_enabled": false,
                "acl_default_allow": false
        }))
        .send()
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
}

pub(crate) fn make_network() -> Value {
    json!({
        "name": "network",
        "address": "10.1.1.1/24",
        "port": 55555,
        "endpoint": "192.168.4.14",
        "allowed_ips": "10.1.1.0/24",
        "dns": "1.1.1.1",
        "allowed_groups": [],
        "mfa_enabled": false,
        "keepalive_interval": 25,
        "peer_disconnect_threshold": 180,
        "acl_enabled": false,
        "acl_default_allow": false
    })
}

/// Replaces id field in json response with NoId
pub(crate) fn omit_id<T: DeserializeOwned>(mut value: Value) -> T {
    *value.get_mut("id").unwrap() = json!(NoId);
    serde_json::from_value(value).unwrap()
}

pub(crate) async fn make_client(pool: PgPool) -> TestClient {
    let (client, _) = make_test_client(pool).await;
    client
}

pub(crate) async fn make_client_with_db(pool: PgPool) -> (TestClient, PgPool) {
    let (client, client_state) = make_test_client(pool).await;
    (client, client_state.pool)
}

pub(crate) async fn make_client_with_state(pool: PgPool) -> (TestClient, ClientState) {
    let (client, client_state) = make_test_client(pool).await;
    (client, client_state)
}
