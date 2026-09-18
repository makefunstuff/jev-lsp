//! Fetching a page a model asked for.
//!
//! This is the one place the server reaches the network on a model's behalf, so the shape is
//! deliberately narrow: https only, a size cap, a timeout, and no redirect following. A model
//! that could pull arbitrary bytes into its own prompt is a prompt-injection path, and a user
//! who cannot see which URL was read cannot judge the answer.

/// Largest page handed back to the model. Documentation fits; a binary dump does not.
pub const MAX_BYTES: usize = 64 * 1024;

/// How long a fetch may take before it is abandoned.
pub const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Fetch a page as text, or say why not.
pub fn page(url: &str) -> Result<String, String> {
    if !url.starts_with("https://") {
        return Err("only https is fetched".to_string());
    }
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .max_redirects(0)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut response = agent
        .get(url)
        .call()
        .map_err(|e| format!("the fetch failed: {e}"))?;
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("the page could not be read as text: {e}"))?;
    if body.is_empty() {
        return Err("the page was empty".to_string());
    }
    if body.len() > MAX_BYTES {
        return Ok(body.chars().take(MAX_BYTES).collect());
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::page;

    #[test]
    fn a_plain_http_url_is_refused_before_any_request() {
        let err = page("http://example.com").unwrap_err();
        assert!(err.contains("https"), "the reason names the rule: {err}");
    }

    #[test]
    fn a_scheme_that_is_not_http_is_refused() {
        assert!(page("file:///etc/passwd").is_err());
        assert!(page("gopher://example.com").is_err());
    }
}
