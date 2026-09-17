//! Advertise a capability the pinned `lsp-types` cannot express.
//!
//! `tower-lsp` 0.20 pins `lsp-types` 0.94, whose `ServerCapabilities` has no
//! `inlineCompletionProvider` field: that arrived with the 3.18 draft and first appears in
//! `lsp-types` 0.95. The server genuinely serves `textDocument/inlineCompletion`, but Neovim
//! only attaches its inline-completion handler for a client that advertises the capability
//! (`vim/lsp/_capability.lua:145` asks `supports_method` before `on_attach`, and
//! `vim/lsp/protocol.lua:1273` maps the method to that field). Without it the handler would
//! be dead code no client could reach.
//!
//! So the field is injected into the `initialize` response at the transport boundary. That is
//! the smallest honest fix: the alternative is abandoning the feature, and the other
//! alternative — moving to a release-candidate framework for one field — would break every
//! handler for a draft capability.
//!
//! This is the only place the wire is touched outside `LanguageServer`.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tower::Service;
use tower_lsp::jsonrpc::{Request, Response};

/// Wraps the LSP service and adds `inlineCompletionProvider` to the `initialize` result.
#[derive(Clone)]
pub struct Advertised<S> {
    inner: S,
}

impl<S> Advertised<S> {
    pub fn new(inner: S) -> Self {
        Advertised { inner }
    }
}

impl<S> Service<Request> for Advertised<S>
where
    S: Service<Request, Response = Option<Response>> + Send + 'static,
    S::Future: Send,
{
    type Response = Option<Response>;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let is_initialize = request.method() == "initialize";
        let inner = self.inner.call(request);
        Box::pin(async move {
            let response = inner.await?;
            Ok(response.map(|r| if is_initialize { with_inline_completion(r) } else { r }))
        })
    }
}

/// Add the field if the response is a successful `initialize` result that lacks it.
fn with_inline_completion(response: Response) -> Response {
    let (id, body) = response.into_parts();
    let body = match body {
        Ok(mut value) => {
            if let Some(capabilities) = value
                .get_mut("capabilities")
                .and_then(|c| c.as_object_mut())
            {
                capabilities
                    .entry("inlineCompletionProvider".to_string())
                    .or_insert_with(|| serde_json::json!({}));
            }
            Ok(value)
        }
        // An initialize that failed is left alone: there is nothing to advertise.
        Err(error) => Err(error),
    };
    Response::from_parts(id, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn response(result: serde_json::Value) -> Response {
        Response::from_parts(tower_lsp::jsonrpc::Id::Number(1), Ok(result))
    }

    #[test]
    fn the_capability_is_added_to_an_initialize_result() {
        let r = with_inline_completion(response(json!({
            "capabilities": {"positionEncoding": "utf-8"},
            "serverInfo": {"name": "meta-lsp"}
        })));
        let (_, body) = r.into_parts();
        let value = body.expect("still a success");
        assert_eq!(value["capabilities"]["inlineCompletionProvider"], json!({}));
        assert_eq!(value["capabilities"]["positionEncoding"], "utf-8");
        assert_eq!(value["serverInfo"]["name"], "meta-lsp");
    }

    #[test]
    fn an_existing_value_is_never_overwritten() {
        let r = with_inline_completion(response(json!({
            "capabilities": {"inlineCompletionProvider": {"something": true}}
        })));
        let (_, body) = r.into_parts();
        assert_eq!(
            body.unwrap()["capabilities"]["inlineCompletionProvider"]["something"],
            true
        );
    }

    #[test]
    fn a_result_without_capabilities_is_left_alone() {
        let r = with_inline_completion(response(json!({"unexpected": true})));
        let (_, body) = r.into_parts();
        assert_eq!(body.unwrap(), json!({"unexpected": true}));
    }

    #[test]
    fn an_error_response_is_passed_through() {
        let error = tower_lsp::jsonrpc::Error::invalid_params("nope");
        let r = with_inline_completion(Response::from_parts(
            tower_lsp::jsonrpc::Id::Number(1),
            Err(error),
        ));
        let (_, body) = r.into_parts();
        assert!(body.is_err(), "an error stays an error");
    }
}
