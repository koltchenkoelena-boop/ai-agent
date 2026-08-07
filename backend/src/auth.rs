use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, RedirectUrl, Scope, TokenResponse,
    TokenUrl,
};
use oauth2::basic::BasicClient;
use actix_web::{web, HttpResponse, Responder};
use serde::Deserialize;
use uuid::Uuid;
use crate::config::Settings;

pub struct GoogleAuth {
    pub client: BasicClient,
}

impl GoogleAuth {
    pub fn new(settings: &Settings) -> Self {
        let client = BasicClient::new(
            ClientId::new(settings.google_client_id.clone()),
            Some(ClientSecret::new(settings.google_client_secret.clone())),
            AuthUrl::new("https://accounts.google.com/o/oauth2/auth".to_string()).unwrap(),
            Some(TokenUrl::new("https://oauth2.googleapis.com/token").unwrap()),
        )
        .set_redirect_uri(RedirectUrl::new(settings.google_redirect_uri.clone()).unwrap());
        GoogleAuth { client }
    }

    pub fn authorize_url(&self) -> (String, CsrfToken) {
        let (auth_url, csrf_token) = self
            .client
            .authorize_url(CsrfToken::new_random)
            .add_scope(Scope::new(
                "https://www.googleapis.com/auth/userinfo.email".to_string(),
            ))
            .add_scope(Scope::new(
                "https://www.googleapis.com/auth/userinfo.profile".to_string(),
            ))
            .url();
        (auth_url.to_string(), csrf_token)
    }

    pub async fn handle_callback(
        &self,
        query: web::Query<GoogleCallback>,
        session: actix_session::Session,
    ) -> impl Responder {
        if let Some(state) = query.state.clone() {
            let mut session_csrf_token: Option<String> = session.get("csrf_token").unwrap_or(None);
            if let Some(stored_state) = session_csrf_token {
                if state != stored_state {
                    return HttpResponse::BadRequest().body("Invalid state parameter");
                }
            } else {
                return HttpResponse::BadRequest().body("CSRF token not found in session");
            }
        } else {
            return HttpResponse::BadRequest().body("Missing state parameter");
        }

        let token_result = self
            .client
            .exchange_code(AuthorizationCode::new(query.code.clone()))
            .request_async(oauth2::reqwest::async_http_client)
            .await;

        match token_result {
            Ok(token) => {
                let user_info = self
                    .get_user_info(token.access_token())
                    .await
                    .map_err(|e| {
                        eprintln!("Failed to get user info: {}", e);
                        HttpResponse::InternalServerError().body("Failed to get user info")
                    })?;

                // Store user info in session (simplified)
                session.insert("user", &user_info).unwrap();

                HttpResponse::Found()
                    .header(actix_http::header::LOCATION, "/")
                    .finish()
            }
            Err(e) => {
                eprintln!("Token exchange error: {}", e);
                HttpResponse::BadRequest().body("Failed to exchange code for token")
            }
        }
    }

    async fn get_user_info(&self, access_token: &str) -> Result<GoogleUserInfo, reqwest::Error> {
        let client = reqwest::Client::new();
        let resp = client
            .get("https://www.googleapis.com/oauth2/v2/userinfo")
            .bearer_token(access_token)
            .send()
            .await?
            .json::<GoogleUserInfo>()
            .await?;
        Ok(resp)
    }
}

#[derive(Debug, Deserialize)]
pub struct GoogleCallback {
    code: String,
    state: String,
}

#[derive(Debug, Deserialize)]
struct GoogleUserInfo {
    id: String,
    email: String,
    verified_email: bool,
    name: String,
    given_name: String,
    family_name: String,
    picture: String,
    locale: String,
}