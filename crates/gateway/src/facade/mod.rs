//! Facade registry and handler abstraction for Kiro IDE endpoints.
//!
//! Compliant with Spec §15.1 (Pluggable Facade Handler Registry).

pub use axum::response::Response;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    response::IntoResponse,
    routing::MethodRouter,
    Router,
};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub mod admin;
pub mod admin_login;
pub mod client;
pub mod commercial;
pub mod completions;
pub mod conversation;
pub mod healthz;
pub mod mcp;
pub mod models;
mod modern;
pub mod oauth;
pub mod portal;
pub mod profiles;
pub mod provider_import;
pub mod subscriptions;
pub mod usage;
pub mod virtualization;

pub use healthz::{HealthzHandler, MetricsHandler};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Uniform handler trait for Kiro endpoints (Spec §15.1).
pub trait FacadeHandler: Send + Sync {
    /// HTTP method matched by this handler.
    fn method(&self) -> Method;

    /// Exact URI path matched by this handler.
    fn path(&self) -> &'static str;

    /// Process the request and generate an HTTP response.
    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response>;
}

/// Structured AWS JSON error response helper.
pub fn error_response(status: StatusCode, error_type: &str, message: &str) -> Response {
    let body = serde_json::json!({
        "__type": error_type,
        "message": message,
    });
    (
        status,
        [("content-type", "application/x-amz-json-1.1")],
        axum::Json(body),
    )
        .into_response()
}

/// Structured JSON success response helper.
pub fn json_response<T: serde::Serialize>(status: StatusCode, data: &T) -> Response {
    (
        status,
        [("content-type", "application/x-amz-json-1.1")],
        axum::Json(data),
    )
        .into_response()
}

/// Fallback handler for unmapped routes, returning structured JSON error instead of 500.
pub async fn fallback_handler(req: Request<Body>) -> Response {
    let method = req.method().to_string();
    let uri = req.uri().to_string();
    error_response(
        StatusCode::NOT_FOUND,
        "ResourceNotFoundException",
        &format!("Cannot {} {}", method, uri),
    )
}

/// Central registry for all Kiro facade handlers.
pub struct FacadeRegistry {
    handlers: Vec<Arc<dyn FacadeHandler>>,
    admin_auth: Option<Arc<admin::AdminAuthState>>,
    runtime: Option<crate::provider::ProviderRuntimeRegistry>,
}

impl Default for FacadeRegistry {
    fn default() -> Self {
        let mut registry = Self::new();
        registry.register_default_facades();
        registry
    }
}

impl FacadeRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
            admin_auth: None,
            runtime: None,
        }
    }

    /// Configure shared provider runtime registry across facade handlers (T04).
    pub fn with_runtime(mut self, runtime: crate::provider::ProviderRuntimeRegistry) -> Self {
        self.runtime = Some(runtime);
        self
    }

    /// Register a handler (Spec §15.1: 1-line registration).
    pub fn register<H: FacadeHandler + 'static>(&mut self, handler: H) -> &mut Self {
        self.handlers.push(Arc::new(handler));
        self
    }

    /// Register all standard Spec §4.2 Kiro facades with a custom virtualization store.
    pub fn register_virtualized_facades(
        &mut self,
        store: virtualization::VirtualizationStore,
    ) -> &mut Self {
        self.register(healthz::MetricsHandler::default())
            .register(oauth::OAuthTokenHandler::default())
            .register(oauth::RefreshTokenHandler::default())
            .register(client::ClientNegotiateHandler)
            .register(client::ClientBeaconHandler)
            .register(client::ClientBrandHandler::default())
            .register(models::ListAvailableModelsHandler::new(store.clone()))
            .register(usage::GetUsageLimitsHandler::new(store.clone()))
            .register(subscriptions::ListAvailableSubscriptionsHandler::new(
                store.clone(),
            ))
            .register(subscriptions::CreateSubscriptionTokenHandler)
            .register(profiles::ListAvailableProfilesHandler::new(store.clone()));
        let mut conv = conversation::GenerateAssistantResponseHandler::default();
        if let Some(ref rt) = self.runtime {
            conv = conv.with_runtime(rt.clone());
        }
        self.register(conv)
            .register(completions::GenerateCompletionsHandler::default())
            .register(mcp::McpHandler::default());
        // self.register_portal_facades(b, Some(store)); // ponytail: removed double registration
        self
    }

    /// Register end-user web self-service portal endpoints (Spec §14.2, P4-9).
    pub fn register_portal_facades(
        &mut self,
        billing: billing::BillingEngine,
        store: Option<virtualization::VirtualizationStore>,
    ) -> &mut Self {
        let rate_limiter = crate::security::IpRateLimiter::default();
        let protector = crate::security::BruteForceProtector::default();
        let challenge_mgr = crate::security::PortalChallengeManager::default();

        self.register(client::AnnouncementsHandler {
            billing: billing.clone(),
        });
        self.register(portal::PortalChallengeHandler {
            challenge_mgr: challenge_mgr.clone(),
            rate_limiter: rate_limiter.clone(),
        })
        .register(portal::PortalQueryHandler {
            billing: billing.clone(),
            store,
            rate_limiter: rate_limiter.clone(),
        })
        .register(portal::PortalActivateHandler {
            billing: billing.clone(),
            rate_limiter: rate_limiter.clone(),
        })
        .register(portal::PortalUnbindHandler {
            billing: billing.clone(),
            rate_limiter: rate_limiter.clone(),
            protector: protector.clone(),
            challenge_mgr: challenge_mgr.clone(),
        })
        .register(portal::PortalTopupHandler {
            billing,
            rate_limiter,
            protector,
            challenge_mgr,
        })
        .register(portal::PortalWebPageHandler);
        self
    }

    /// Register Administrator REST API endpoints (Spec §7, P0-01, P0-02, P1-02).
    pub fn register_admin_facades(
        &mut self,
        billing: billing::BillingEngine,
        admin_key: String,
    ) -> &mut Self {
        self.register_admin_facades_with_auth(
            billing,
            Arc::new(admin::AdminAuthState::new(admin_key)),
        )
    }

    /// Register the production administrator surface. Production routes accept
    /// only short-lived admin sessions; the bootstrap key is limited to the
    /// session issuance endpoint.
    pub fn register_admin_facades_secure(
        &mut self,
        billing: billing::BillingEngine,
        admin_key: String,
    ) -> &mut Self {
        self.register_admin_facades_with_auth(
            billing,
            Arc::new(admin::AdminAuthState::new_production(admin_key)),
        )
    }

    fn register_admin_facades_with_auth(
        &mut self,
        billing: billing::BillingEngine,
        auth: Arc<admin::AdminAuthState>,
    ) -> &mut Self {
        self.admin_auth = Some(auth.clone());
        let runtime = self.runtime.clone();
        self.register(admin::AdminSessionHandler { auth: auth.clone() })
            .register(admin::AdminRevokeSessionsHandler { auth: auth.clone() })
            .register(admin::AdminMeHandler { auth: auth.clone() })
            .register(admin::AdminStatsHandler {
                billing: billing.clone(),
                auth: auth.clone(),
            })
            .register(admin::AdminCardsHandler {
                billing: billing.clone(),
                auth: auth.clone(),
            })
            .register(admin::AdminCardRevealHandler {
                billing: billing.clone(),
                auth: auth.clone(),
            })
            .register(admin::AdminCardStatusHandler {
                billing: billing.clone(),
                auth: auth.clone(),
            })
            .register(admin::AdminCardAdjustHandler {
                billing: billing.clone(),
                auth: auth.clone(),
            })
            .register(admin::AdminGetAnnouncementsHandler {
                billing: billing.clone(),
                auth: auth.clone(),
            })
            .register(admin::AdminFinancialsHandler {
                billing: billing.clone(),
                auth: auth.clone(),
            })
            .register(commercial::CommercialHandler {
                billing: billing.clone(),
                auth: auth.clone(),
                publish: false,
            })
            .register(commercial::CommercialHandler {
                billing: billing.clone(),
                auth: auth.clone(),
                publish: true,
            })
            .register(commercial::ProviderKeysHandler {
                billing: billing.clone(),
                auth: auth.clone(),
                runtime: runtime.clone(),
                discover: false,
            })
            .register(commercial::ProviderKeysHandler {
                billing: billing.clone(),
                auth: auth.clone(),
                runtime: runtime.clone(),
                discover: true,
            })
            .register(admin::AdminProvidersHandler {
                billing: billing.clone(),
                auth: auth.clone(),
            })
            .register(admin::AdminProviderStatusHandler {
                billing: billing.clone(),
                auth: auth.clone(),
                runtime,
            })
            .register(admin::AdminTracesHandler {
                billing: billing.clone(),
                auth: auth.clone(),
            })
            .register(admin::AdminLedgerExportHandler {
                billing: billing.clone(),
                auth: auth.clone(),
                json: true,
            })
            .register(admin::AdminLedgerExportHandler {
                billing: billing.clone(),
                auth: auth.clone(),
                json: false,
            })
            .register(admin::AdminPruneTracesHandler {
                billing: billing.clone(),
                auth: auth.clone(),
            })
            .register(admin::AdminArchiveLedgerHandler {
                billing: billing.clone(),
                auth: auth.clone(),
            })
            .register(admin::AdminBatchCardsHandler {
                billing: billing.clone(),
                auth: auth.clone(),
            })
            .register(admin::AdminCreateAnnouncementHandler {
                billing: billing.clone(),
                auth: auth.clone(),
            })
            .register(admin::AdminSnapshotSyncHandler { billing, auth });
        self
    }

    /// Register provider import only after an administrator trust boundary has
    /// been configured.  Keeping this out of the default facade set prevents a
    /// test/demo registry from accidentally becoming the production route map.
    pub fn register_provider_import_facade(
        &mut self,
        store: virtualization::VirtualizationStore,
        billing: billing::BillingEngine,
    ) -> &mut Self {
        if let Some(auth) = self.admin_auth.clone() {
            let mut handler = provider_import::ProviderImportHandler::new()
                .with_store(store)
                .with_billing(billing)
                .with_admin_auth(auth);
            if let Some(ref rt) = self.runtime {
                handler = handler.with_runtime(rt.clone());
            }
            self.register(handler);
        }
        self
    }

    /// Register all standard Spec §4.2 Kiro facades with default virtualization store.
    pub fn register_default_facades(&mut self) -> &mut Self {
        self.register_virtualized_facades(virtualization::VirtualizationStore::default())
            .register(healthz::HealthzHandler::default())
    }

    /// Get slice of currently registered handlers.
    pub fn handlers(&self) -> &[Arc<dyn FacadeHandler>] {
        &self.handlers
    }

    /// Convert the registry into an Axum router with fallback structured errors.
    pub fn into_router(mut self) -> Router {
        modern::register(&mut self.handlers);
        let mut by_route: HashMap<(&'static str, Method), Arc<dyn FacadeHandler>> = HashMap::new();
        for h in self.handlers {
            by_route.insert((h.path(), h.method()), h);
        }

        let mut by_path: HashMap<&'static str, Vec<Arc<dyn FacadeHandler>>> = HashMap::new();
        for ((path, _), h) in by_route {
            by_path.entry(path).or_default().push(h);
        }

        let mut router = Router::new();

        for (path, handlers) in by_path {
            let mut method_router = MethodRouter::new();
            for h in handlers {
                let h_cloned = Arc::clone(&h);
                let method = h.method();
                method_router = match method {
                    Method::GET => method_router.get(move |req: Request<Body>| {
                        let h = Arc::clone(&h_cloned);
                        async move { h.handle(req).await }
                    }),
                    Method::POST => method_router.post(move |req: Request<Body>| {
                        let h = Arc::clone(&h_cloned);
                        async move { h.handle(req).await }
                    }),
                    Method::PUT => method_router.put(move |req: Request<Body>| {
                        let h = Arc::clone(&h_cloned);
                        async move { h.handle(req).await }
                    }),
                    Method::DELETE => method_router.delete(move |req: Request<Body>| {
                        let h = Arc::clone(&h_cloned);
                        async move { h.handle(req).await }
                    }),
                    Method::PATCH => method_router.patch(move |req: Request<Body>| {
                        let h = Arc::clone(&h_cloned);
                        async move { h.handle(req).await }
                    }),
                    _ => panic!("Unsupported HTTP method for facade handler: {}", method),
                };
            }
            router = router.route(path, method_router);
        }

        router.fallback(fallback_handler)
    }

    /// Convert the registry into an Axum router protected by AuthState middleware.
    /// Excludes public auth endpoints (/oauth/token, /refreshToken) from requiring Bearer tokens.
    pub fn into_router_with_auth(mut self, auth: crate::auth::AuthState) -> Router {
        modern::register(&mut self.handlers);
        let admin_auth = self
            .admin_auth
            .unwrap_or_else(|| Arc::new(admin::AdminAuthState::new(String::new())));
        let mut public_routes: HashMap<(&'static str, Method), Arc<dyn FacadeHandler>> =
            HashMap::new();
        let mut protected_routes: HashMap<(&'static str, Method), Arc<dyn FacadeHandler>> =
            HashMap::new();
        let mut admin_routes: HashMap<(&'static str, Method), Arc<dyn FacadeHandler>> =
            HashMap::new();

        for h in self.handlers {
            let path = h.path();
            if path == "/healthz"
                || path == "/oauth/token"
                || path == "/oauth/token/refresh"
                || path == "/refreshToken"
                || path == "/client/negotiate"
                || path == "/api/v1/announcements"
                || path == "/client/beacon"
                || path == "/client/brand"
                || path == "/portal"
                || path.starts_with("/api/v1/portal/")
            {
                public_routes.insert((path, h.method()), h);
            } else if path == "/metrics" || path.starts_with("/api/v1/admin/") {
                admin_routes.insert((path, h.method()), h);
            } else {
                protected_routes.insert((path, h.method()), h);
            }
        }

        // Build protected router and apply auth_middleware
        let mut protected_by_path: HashMap<&'static str, Vec<Arc<dyn FacadeHandler>>> =
            HashMap::new();
        for ((path, _), h) in protected_routes {
            protected_by_path.entry(path).or_default().push(h);
        }

        let mut protected_router = Router::new();
        for (path, handlers) in protected_by_path {
            let mut method_router = MethodRouter::new();
            for h in handlers {
                let h_cloned = Arc::clone(&h);
                let method = h.method();
                method_router = match method {
                    Method::GET => method_router.get(move |req: Request<Body>| {
                        let h = Arc::clone(&h_cloned);
                        async move { h.handle(req).await }
                    }),
                    Method::POST => method_router.post(move |req: Request<Body>| {
                        let h = Arc::clone(&h_cloned);
                        async move { h.handle(req).await }
                    }),
                    Method::PUT => method_router.put(move |req: Request<Body>| {
                        let h = Arc::clone(&h_cloned);
                        async move { h.handle(req).await }
                    }),
                    Method::DELETE => method_router.delete(move |req: Request<Body>| {
                        let h = Arc::clone(&h_cloned);
                        async move { h.handle(req).await }
                    }),
                    Method::PATCH => method_router.patch(move |req: Request<Body>| {
                        let h = Arc::clone(&h_cloned);
                        async move { h.handle(req).await }
                    }),
                    _ => panic!("Unsupported HTTP method for facade handler: {}", method),
                };
            }
            protected_router = protected_router.route(path, method_router);
        }

        let protected_router = protected_router.layer(axum::middleware::from_fn_with_state(
            auth,
            crate::auth::auth_middleware,
        ));

        // Build the administrator router separately so a newly registered admin endpoint
        // cannot accidentally inherit the public-router trust boundary.
        let mut admin_by_path: HashMap<&'static str, Vec<Arc<dyn FacadeHandler>>> = HashMap::new();
        for ((path, _), h) in admin_routes {
            admin_by_path.entry(path).or_default().push(h);
        }

        let mut admin_router = Router::new();
        for (path, handlers) in admin_by_path {
            let mut method_router = MethodRouter::new();
            for h in handlers {
                let h_cloned = Arc::clone(&h);
                let method = h.method();
                method_router = match method {
                    Method::GET => method_router.get(move |req: Request<Body>| {
                        let h = Arc::clone(&h_cloned);
                        async move { h.handle(req).await }
                    }),
                    Method::POST => method_router.post(move |req: Request<Body>| {
                        let h = Arc::clone(&h_cloned);
                        async move { h.handle(req).await }
                    }),
                    Method::PUT => method_router.put(move |req: Request<Body>| {
                        let h = Arc::clone(&h_cloned);
                        async move { h.handle(req).await }
                    }),
                    Method::DELETE => method_router.delete(move |req: Request<Body>| {
                        let h = Arc::clone(&h_cloned);
                        async move { h.handle(req).await }
                    }),
                    Method::PATCH => method_router.patch(move |req: Request<Body>| {
                        let h = Arc::clone(&h_cloned);
                        async move { h.handle(req).await }
                    }),
                    _ => panic!("Unsupported HTTP method for facade handler: {}", method),
                };
            }
            admin_router = admin_router.route(path, method_router);
        }

        let admin_router = admin_router
            .layer(axum::middleware::from_fn_with_state(
                admin_auth,
                crate::facade::admin::admin_auth_middleware,
            ))
            .layer(axum::middleware::from_fn(admin::card_secret_no_store));

        // Build public router
        let mut public_by_path: HashMap<&'static str, Vec<Arc<dyn FacadeHandler>>> = HashMap::new();
        for ((path, _), h) in public_routes {
            public_by_path.entry(path).or_default().push(h);
        }

        let mut public_router = Router::new();
        for (path, handlers) in public_by_path {
            let mut method_router = MethodRouter::new();
            for h in handlers {
                let h_cloned = Arc::clone(&h);
                let method = h.method();
                method_router = match method {
                    Method::GET => method_router.get(move |req: Request<Body>| {
                        let h = Arc::clone(&h_cloned);
                        async move { h.handle(req).await }
                    }),
                    Method::POST => method_router.post(move |req: Request<Body>| {
                        let h = Arc::clone(&h_cloned);
                        async move { h.handle(req).await }
                    }),
                    _ => panic!("Unsupported HTTP method for facade handler: {}", method),
                };
            }
            public_router = public_router.route(path, method_router);
        }

        public_router
            .merge(protected_router)
            .merge(admin_router)
            .fallback(fallback_handler)
    }
}
