//! Resolve public Codex Pets page links without sending local pet files anywhere.
use reqwest::Url;
use serde_json::Value;
use std::time::Duration;

pub(super) fn slug_from_link(link: &str) -> Option<String> {
    let url = Url::parse(link).ok()?;
    if url.scheme() != "https"
        || url.host_str() != Some("codex-pets.net")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
    {
        return None;
    }
    let slug = if let Some(slug) = url.path().strip_prefix("/share/") {
        if url.fragment().is_some() {
            return None;
        }
        slug
    } else if url.path() == "/" {
        url.fragment()?.strip_prefix("/pets/")?
    } else {
        return None;
    };
    if slug.is_empty()
        || slug.len() > 100
        || slug.starts_with('-')
        || slug.ends_with('-')
        || slug.contains("--")
        || !slug
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    {
        return None;
    }
    Some(slug.to_owned())
}

fn poster_from_response(slug: &str, value: &Value) -> Option<String> {
    let pet = value.get("pet")?;
    if pet.get("id")?.as_str()? != slug {
        return None;
    }
    let image = pet.get("posterUrl")?.as_str()?;
    let url = Url::parse(image).ok()?;
    if url.scheme() != "https"
        || url.host_str() != Some("codex-pets.net")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.path().starts_with("/assets/pets/")
        || !url
            .path()
            .ends_with(format!("/{slug}/poster.webp").as_str())
    {
        return None;
    }
    Some(image.to_owned())
}

/// Return the site's publicly hosted static poster, not a local sprite atlas.
pub async fn resolve(link: &str) -> Result<String, String> {
    let slug = slug_from_link(link).ok_or_else(|| "invalid Codex Pets link".to_owned())?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| "could not create client")?;
    let mut response = client
        .get(format!("https://codex-pets.net/api/pets/{slug}"))
        .send()
        .await
        .map_err(|_| "could not reach Codex Pets")?
        .error_for_status()
        .map_err(|_| "pet not found")?;
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "could not read pet")? {
        if body.len().saturating_add(chunk.len()) > 64 * 1024 {
            return Err("pet response too large".into());
        }
        body.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&body).map_err(|_| "invalid pet response")?;
    poster_from_response(&slug, &value).ok_or_else(|| "pet has no public poster".into())
}

/// Check the generated GIF before handing its URL to Discord. The fixed
/// service origin prevents this health check from fetching arbitrary URLs.
pub async fn animated_image_available(image: &str) -> bool {
    let Ok(url) = Url::parse(image) else {
        return false;
    };
    if url.scheme() != "https"
        || url.host_str() != Some("win-codexbar-pet-images.vercel.app")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/api/pet"
        || url.fragment().is_some()
    {
        return false;
    }
    let Ok(client) = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(6))
        .build()
    else {
        return false;
    };
    let Ok(mut response) = client.get(url).send().await else {
        return false;
    };
    if !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|size| size > 4 * 1024 * 1024)
        || !response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(';')
                    .next()
                    .is_some_and(|v| v.trim().eq_ignore_ascii_case("image/gif"))
            })
    {
        return false;
    }
    let mut prefix = Vec::with_capacity(6);
    while prefix.len() < 6 {
        let Ok(Some(chunk)) = response.chunk().await else {
            return false;
        };
        let take = (6 - prefix.len()).min(chunk.len());
        prefix.extend_from_slice(&chunk[..take]);
    }
    is_gif_header(&prefix)
}

fn is_gif_header(bytes: &[u8]) -> bool {
    bytes.starts_with(b"GIF89a") || bytes.starts_with(b"GIF87a")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_only_gif_signatures() {
        assert!(is_gif_header(b"GIF89a...."));
        assert!(is_gif_header(b"GIF87a...."));
        assert!(!is_gif_header(b"<html>"));
    }

    #[test]
    fn accepts_only_site_pet_links() {
        for link in [
            "https://codex-pets.net/share/astro-burn",
            "https://codex-pets.net/#/pets/astro-burn",
        ] {
            assert_eq!(slug_from_link(link).as_deref(), Some("astro-burn"));
        }
        for link in [
            "https://codex-pets.net.evil.test/share/x",
            "https://user@codex-pets.net/share/x",
            "http://codex-pets.net/share/x",
            "https://codex-pets.net/share/a/b",
            "https://codex-pets.net/#/pets/a--b",
        ] {
            assert!(slug_from_link(link).is_none());
        }
    }

    #[test]
    fn accepts_only_matching_public_posters() {
        let good = json!({"pet":{"id":"astro-burn","posterUrl":"https://codex-pets.net/assets/pets/v/123/astro-burn/poster.webp"}});
        assert!(poster_from_response("astro-burn", &good).is_some());
        assert!(poster_from_response("other", &good).is_none());
        let bad = json!({"pet":{"id":"astro-burn","posterUrl":"https://evil.test/assets/pets/astro-burn/poster.webp"}});
        assert!(poster_from_response("astro-burn", &bad).is_none());
    }
}
