use std::borrow::Cow;
use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::Result;
use async_graphql::http::{playground_source, GraphQLPlaygroundConfig};
use async_graphql::ServerError;
use hyper::{Body, HeaderMap, Request, Response, StatusCode};
use serde::de::DeserializeOwned;

use super::request_context::RequestContext;
use super::AppContext;
use crate::async_graphql_hyper::{GraphQLRequestLike, GraphQLResponse};

pub fn graphiql(req: &Request<Body>) -> Result<Response<Body>> {
    let query = req.uri().query();
    let endpoint = "/graphql";
    let endpoint = if let Some(query) = query {
        if query.is_empty() {
            Cow::Borrowed(endpoint)
        } else {
            Cow::Owned(format!("{}?{}", endpoint, query))
        }
    } else {
        Cow::Borrowed(endpoint)
    };

    Ok(Response::new(Body::from(playground_source(
        GraphQLPlaygroundConfig::new(&endpoint).title("Tailcall - GraphQL IDE"),
    ))))
}

fn not_found() -> Result<Response<Body>> {
    Ok(Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::empty())?)
}

fn create_request_context(req: &Request<Body>, server_ctx: &AppContext) -> RequestContext {
    let upstream = server_ctx.blueprint.upstream.clone();
    let allowed = upstream.allowed_headers;
    let headers = create_allowed_headers(req.headers(), &allowed);
    RequestContext::from(server_ctx).req_headers(headers)
}

fn update_cache_control_header(
    response: GraphQLResponse,
    server_ctx: &AppContext,
    req_ctx: Arc<RequestContext>,
) -> GraphQLResponse {
    if server_ctx.blueprint.server.enable_cache_control_header {
        let ttl = req_ctx.get_min_max_age().unwrap_or(0);
        let cache_public_flag = req_ctx.is_cache_public().unwrap_or(true);
        return response.set_cache_control(ttl, cache_public_flag);
    }
    response
}

pub fn update_response_headers(resp: &mut hyper::Response<hyper::Body>, server_ctx: &AppContext) {
    if !server_ctx.blueprint.server.response_headers.is_empty() {
        resp.headers_mut()
            .extend(server_ctx.blueprint.server.response_headers.clone());
    }
}

pub async fn graphql_request<T: DeserializeOwned + GraphQLRequestLike>(
    req: Request<Body>,
    server_ctx: &AppContext,
) -> Result<Response<Body>> {
    let req_ctx = Arc::new(create_request_context(&req, server_ctx));
    let bytes = hyper::body::to_bytes(req.into_body()).await?;
    let request = serde_json::from_slice::<T>(&bytes);
    match request {
        Ok(request) => {
            let mut response = request
                .data(req_ctx.clone())
                .execute(&server_ctx.schema)
                .await;
            response = update_cache_control_header(response, server_ctx, req_ctx);
            let mut resp = response.to_response()?;
            update_response_headers(&mut resp, server_ctx);
            Ok(resp)
        }
        Err(err) => {
            log::error!(
                "Failed to parse request: {}",
                String::from_utf8(bytes.to_vec()).unwrap()
            );

            let mut response = async_graphql::Response::default();
            let server_error =
                ServerError::new(format!("Unexpected GraphQL Request: {}", err), None);
            response.errors = vec![server_error];

            Ok(GraphQLResponse::from(response).to_response()?)
        }
    }
}

fn create_allowed_headers(headers: &HeaderMap, allowed: &BTreeSet<String>) -> HeaderMap {
    let mut new_headers = HeaderMap::new();
    for (k, v) in headers.iter() {
        if allowed.contains(k.as_str()) {
            new_headers.insert(k, v.clone());
        }
    }

    new_headers
}

pub async fn handle_request<T: DeserializeOwned + GraphQLRequestLike>(
    req: Request<Body>,
    state: Arc<AppContext>,
) -> Result<Response<Body>> {
    match *req.method() {
        hyper::Method::POST if req.uri().path().ends_with("/graphql") => {
            graphql_request::<T>(req, state.as_ref()).await
        }
        hyper::Method::GET if state.blueprint.server.enable_graphiql => graphiql(&req),
        _ => not_found(),
    }
}
