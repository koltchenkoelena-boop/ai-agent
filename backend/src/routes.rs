use actix_web::{web, HttpResponse, Responder, Error};
use actix_session::Session;
use serde_json::json;
use crate::auth::GoogleAuth;

pub async fn google_login(session: Session) -> Result<HttpResponse, Error> {
    let auth = GoogleAuth::new();
    let (auth_url, csrf_token, pkce_verifier) = auth.generate_auth_url();

    // Store CSRF token and PKCE verifier in session for validation in callback
    session.insert("oauth_csrf_token", csrf_token.secret().to_string())?;
    session.insert("oauth_pkce_verifier", pkce_verifier.secret().to_string())?;

    Ok(HttpResponse::Found()
        .header(actix_web::header::LOCATION, auth_url)
        .finish())
}

pub async fn google_callback(
    session: Session,
    query: web::Query<serde_json::Value>,
) -> Result<HttpResponse, Error> {
    let params = query.into_inner();
    let code = params.get("code")
        .and_then(|c| c.as_str())
        .ok_or_else(|| actix_web::error::ErrorBadRequest("Missing code parameter"))?;
    
    let state = params.get("state")
        .and_then(|s| s.as_str())
        .ok_or_else(|| actix_web::error::ErrorBadRequest("Missing state parameter"))?;

    // Retrieve CSRF token and PKCE verifier from session
    let stored_csrf: Option<String> = session.get("oauth_csrf_token")?;
    let stored_pkce: Option<String> = session.get("oauth_pkce_verifier")?;

    // Remove the tokens from session to prevent replay
    session.purge();

    // Validate state (CSRF token)
    if stored_csrf.as_deref() != Some(state) {
        return Err(actix_web::error::ErrorBadRequest("Invalid state parameter"));
    }

    let auth = GoogleAuth::new();
    let pkce_verifier = oauth2::PkceCodeVerifier::new(stored_pkce.unwrap_or_default());

    match auth.exchange_code(code, pkce_verifier).await {
        Ok(user_info) => {
            // Store user info in session
            session.insert("user", &user_info)?;
            // Redirect to frontend dashboard or home
            Ok(HttpResponse::Found()
                .header(actix_web::header::LOCATION, "/")
                .finish())
        }
        Err(e) => {
            eprintln!("OAuth error: {}", e);
            Err(actix_web::error::ErrorInternalServerError("Authentication failed"))
        }
    }
}

pub async fn get_user(session: Session) -> Result<impl Responder, Error> {
    if let Some(user) = session.get::<serde_json::Value>("user")? {
        Ok(HttpResponse::Ok().json(user))
    } else {
        Ok(HttpResponse::Unauthorized().json(json!({ "error": "Unauthorized" })))
    }
}

pub async fn logout(session: Session) -> Result<HttpResponse, Error> {
    session.purge();
    Ok(HttpResponse::Found()
        .header(actix_web::header::LOCATION, "/")
        .finish())
}

pub fn init(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/api/auth")
            .route("/google/login", web::get().to(google_login))
            .route("/google/callback", web::get().to(google_callback))
    )
    .service(
        web::scope("/api")
            .route("/user", web::get().to(get_user))
            .route("/logout", web::post().to(logout))
    );
}