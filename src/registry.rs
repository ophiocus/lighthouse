//! The property registry — the one place a property is described: its public
//! URL, the path that means "healthy" for its stack, the container that backs
//! it, and the cert to watch. Shipped as a built-in default; a future revision
//! can bootstrap this from `tec-site-apex --list` over SSH so it never drifts.

#[derive(Debug, Clone)]
pub struct PropertyDef {
    pub slug: String,
    pub name: String,
    pub stack: String,
    /// Primary public URL probed for liveness + latency.
    pub url: String,
    /// Path that returns 200 when the property is healthy.
    /// Drupal answers at `/`; the myevery API service answers at `/healthz`.
    pub probe_path: String,
    pub domains: Vec<String>,
    /// Backing container name, matched against the gather's container list.
    pub container: String,
    /// Host whose TLS cert expiry represents this property.
    pub tls_domain: String,
}

fn p(
    slug: &str,
    name: &str,
    stack: &str,
    url: &str,
    probe_path: &str,
    domains: &[&str],
    container: &str,
    tls_domain: &str,
) -> PropertyDef {
    PropertyDef {
        slug: slug.into(),
        name: name.into(),
        stack: stack.into(),
        url: url.into(),
        probe_path: probe_path.into(),
        domains: domains.iter().map(|s| s.to_string()).collect(),
        container: container.into(),
        tls_domain: tls_domain.into(),
    }
}

/// The current Tecnocrática fleet.
pub fn default_registry() -> Vec<PropertyDef> {
    vec![
        p(
            "tecnocratica", "tecnocratica", "Drupal",
            "https://tecnocratica.com.co", "/",
            &["tecnocratica.com.co", "www.tecnocratica.com.co"],
            "tec-tecnocratica-drupal-1", "tecnocratica.com.co",
        ),
        p(
            "monpetitcafe", "monpetitcafe", "Drupal",
            "https://monpetitcafe.com.co", "/",
            &["monpetitcafe.co", "monpetitcafe.com.co", "www.monpetitcafe.co"],
            "tec-monpetitcafe-drupal-1", "monpetitcafe.com.co",
        ),
        p(
            "tempowatch", "tempowatch", "Drupal",
            "https://tempowatch.com.co", "/",
            &["tempowatch.com.co", "www.tempowatch.com.co"],
            "tec-tempowatch-drupal-1", "tempowatch.com.co",
        ),
        p(
            "zero-shot-games", "zero-shot-games", "Drupal",
            "https://zero-shot-games.com", "/",
            &["zero-shot-games.com", "www.zero-shot-games.com"],
            "tec-zero-shot-games-drupal-1", "zero-shot-games.com",
        ),
        p(
            "myevery", "myevery", "Node",
            "https://myevery.tecnocratica.com.co", "/healthz",
            &["myevery.tecnocratica.com.co", "api.myevery.tecnocratica.com.co"],
            "tec-myevery-app-1", "myevery.tecnocratica.com.co",
        ),
    ]
}
