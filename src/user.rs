use askama::Template;
use axum::{
    extract::{Form, Query, RawQuery, State},
    http::header,
    response::{Html, IntoResponse, Redirect},
    Extension,
};
use axum_extra::extract::cookie::Cookie;
use gutp_types::{GutpComment, GutpPost, GutpSubspace, GutpUser};

use crate::redirect_to_error_page;
use crate::AppState;
use crate::HtmlTemplate;
use crate::UserId;
use crate::{make_get, make_post};

#[derive(Template)]
#[template(path = "login_page.html")]
struct LoginPageTemplate {}

pub async fn view_user_login() -> impl IntoResponse {
    HtmlTemplate(LoginPageTemplate {})
}

#[derive(Template)]
#[template(path = "user_info_page.html")]
struct UserInfoPageTemplate {}

pub async fn view_user_info(Extension(user_id): Extension<UserId>) -> impl IntoResponse {
    // TODO: how to get user from middlelayer?
    // retreive user from gutp service

    // has login info
    if let Some(user_id) = user_id {
        // render user info page
        HtmlTemplate(UserInfoPageTemplate {})
    } else {
        // if not logged in, redirect to login page
        let redirect_uri = format!("/login");
        Redirect::to(&redirect_uri)
    }
}

struct GithubOauthCallbackParams {
    code: String,
}

pub async fn login_with_github_callback(
    State(app_state): State<AppState>,
    Query(params): Query<GithubOauthCallbackParams>,
    RawQuery(query): RawQuery,
) -> impl IntoResponse {
    // returned from github
    let code = params.code;

    // get github app client_id and secret
    let client_id = envconfig::get_str_item("GITHUB_APP_CLIENT_ID");
    let client_secret = envconfig::get_str_item("GITHUB_APP_CLIENT_SECRET");

    if let Ok(access_token) = get_github_token(&code, client_id, client_secret).await {
        // use this access_token to retreive user info
        if let Ok(github_user_info) = get_github_user_info(&access_token).await {
            let account = github_user_info.account;
            // now we get user info from github
            // we use the account to check whether this user exist in gutp
            let query = format!("account={}", account);
            let res_bytes = make_get(client, "/v1/user_by_account", Some(query)).await;
            let users: Vec<GutpUser> = serde_json::from_slice(res_bytes).unwrap_or(vec![]);
            let cookie_string;
            if let Some(user) = users.into_iter().next() {
                // if user exists, log it in
                cookie_string = login_user_with_account(&account).await;
            } else {
                // serialize github_user_info to urlencoded string
                // XXX: add oauth source: github
                let body_str = urlencoded::to_str(github_user_info);

                // if user doesn't exist, register it
                let res_bytes = make_post(client, "/v1/user/create", body_str).await;
                let users: Vec<GutpUser> = serde_json::from_slice(res_bytes).unwrap_or(vec![]);
                if let Some(user) = users.into_iter().next() {
                    // registerd successfully
                    cookie_string = login_user_with_account(&account).await;
                } else {
                    // redirect to the error page
                    let action = format!("Register user: {}", account);
                    let err_info = "Unknown";
                    redirect_to_error_page(&action, err_info)
                }
            }
        } else {
            // error on getting github user info
            let action = format!("Get user info from github");
            let err_info = "Failed to get response from github";
            redirect_to_error_page(&action, err_info)
        }
    } else {
        // error on getting github access token
        let action = format!("Get access token from github");
        let err_info = "Failed to request access token from github";
        redirect_to_error_page(&action, err_info)
    }
}

#[derive(Deserialize)]
struct GithubCredentials {
    access_token: String,
}

async fn get_github_token(
    app_state: Arc<AppState>,
    code: &str,
    client_id: &str,
    client_secret: &str,
) -> Result<String, String> {
    let params = [
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("code", code),
        ("accept", "json"),
    ];

    let client = reqwest::Client::new();
    let res = client
        .post("https://github.com/login/oauth/access_token")
        .form(&params)
        .send()
        .await?;

    println!("in get_github_token, res: {:?}", res);

    let status = res.status();
    if !status.is_success() {
        let error = res
            .text()
            .await
            .unwrap_or_else(|_| "Could not get error text".into());
        return Err(anyhow!(format!(
            "got HTTP {}, server said: {}",
            status, error
        )));
    }

    let res_token: GithubCredentials = res.json().await?;
}

#[derive(Deserialize)]
struct GithubUserInfo {
    account: String,
    nickname: String,
    address: String,
}

async fn get_github_user_info(
    app_state: Arc<AppState>,
    access_token: &str,
    account: String,
) -> Result<GithubUserInfo, String> {
    let params = [("access_token", access_token)];
    let token = serde_urlencoded::to_string(params).unwrap();

    let user_url = format!("https://api.github.com/user?{}", token);

    let client = reqwest::Client::new();
    let res = client
        .get(&user_url)
        .header("User-Agent", "gutp-discux")
        .send()
        .await?;

    println!("in get_github_user_info, res: {:?}", res);

    let status = res.status();
    if !status.is_success() {
        let error = res
            .text()
            .await
            .unwrap_or_else(|_| "Could not get error text".into());
        return Err(anyhow!(format!(
            "got HTTP {}, server said: {}",
            status, error
        )));
    }

    let user_info: GithubUserInfo = res.json().await?;
    user_info
}

const TTL: usize = 60 * 24 * 3600;

async fn login_user_with_account(app_state: AppState, account: &str) -> impl IntoResponse {
    // first, set session key in server cache
    let cookiestr = set_session(app_state, account);

    let cookie = Cookie::build("meblog_sid", &cookiestr)
        // .domain("/")
        .path("/")
        //.secure(true)
        .max_age(cookie::time::Duration::seconds(TTL))
        .http_only(true)
        .finish();

    (
        [(header::SET_COOKIE, cookie.to_string())],
        Redirect::to("/"),
    )
}

pub async fn set_session(app_state: AppState, account: &str) -> String {
    let x = rand::random::<[u8; 32]>();
    let cookie = sha256::digest(x);
    let cookie_key = format!("meblog_session:{}", cookie);
    _ = app_state.redis_conn.set(&cookie_key, account).await;
    _ = app_state.redis_conn.expire(&cookie, TTL).await;

    cookie
}
